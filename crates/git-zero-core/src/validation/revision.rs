//! Revision-list history cuts. Boundary membership is independent of PACK membership.
use super::*;

const EXCLUDED: u8 = 1;
const CANDIDATE: u8 = 2;
const PENDING: u8 = 4;
const DONE: u8 = 8;
const BORDER: u8 = 16;
const UNSHALLOW: u8 = 32;

/// Additional limits for an explicitly enabled revision-history request.
#[derive(Clone, Copy, Debug)]
pub struct RevisionLimits {
    /// Maximum exclusion arguments, counting duplicate arguments too.
    pub max_exclusions: usize,
    /// Shared logical-work units, including host ref resolution. Metadata scans
    /// charge their full capacity, index lookups their binary-search upper bound,
    /// and commit-header parsing one unit per byte. Tree/tag traversal reserves
    /// `(index lookup bound + 8)` per permitted structural byte, including rereads.
    /// Unread reservation is released when that operation finishes. This deliberately
    /// conservative accounting bounds work independently of structural reads.
    pub work_limit: u64,
}

/// Revision history, separate from absolute/relative depth. Zero is an active
/// unrestricted revision walk. HTTP resolves exclusion refs to the iterator
/// passed to `select_revision_fetch`; non-commit endpoints have no ancestry effect.
#[derive(Clone, Copy, Debug, Default)]
pub struct RevisionOptions {
    pub since: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShallowChange {
    Shallow(ObjectId),
    Unshallow(ObjectId),
}

/// One caller-owned byte per repository object. Existing metadata/storage layouts
/// remain unchanged. Recreate this descriptor for every request/retry; arrays are
/// reset by selection, and failures invalidate all results.
pub struct RevisionState<'a> {
    states: &'a mut [u8],
    limits: RevisionLimits,
    remaining: u64,
    count: usize,
}

impl<'a> RevisionState<'a> {
    pub fn new(states: &'a mut [u8], limits: RevisionLimits) -> Self {
        Self {
            states,
            limits,
            remaining: limits.work_limit,
            count: 0,
        }
    }

    /// Charge before host-side resolution/comparison work. Sharing this counter
    /// with selection prevents many duplicate names from bypassing graph limits.
    pub fn charge_work(&mut self, units: u64) -> Result<()> {
        charge(&mut self.remaining, units)
    }

    pub fn remaining_work(&self) -> u64 {
        self.remaining
    }
    pub fn max_exclusions(&self) -> usize {
        self.limits.max_exclusions
    }

    /// Changes may include commits absent from the returned PACK. Pass the same
    /// metadata work array used by the successful selection, without modifying it.
    pub fn changes<'b>(
        &'b self,
        objects: &'b [Option<ValidationMetadata>],
    ) -> impl Iterator<Item = ShallowChange> + 'b {
        self.states
            .iter()
            .zip(objects)
            .take(self.count)
            .filter_map(|(&state, object)| {
                let object = object.as_ref()?;
                if state & BORDER != 0 && !object.client_shallow {
                    Some(ShallowChange::Shallow(object.id))
                } else if state & UNSHALLOW != 0 {
                    Some(ShallowChange::Unshallow(object.id))
                } else {
                    None
                }
            })
    }
}

impl<R: Repository + ?Sized> ValidatedRepository<'_, '_, R> {
    /// Select revision-limited history with separate caller-owned boundary state.
    /// `history` contains common commits, client boundaries, and resolved exclusion
    /// endpoints. This additive path rejects an absolute depth, preserves blob
    /// filtering/ascending indices, and shares structural bytes across every pass.
    /// Exclusion and candidate passes visit commits only; payload traversal follows
    /// trees/tags with the existing validated graph contract. No blob body is read.
    /// Existing shallow commits are known, and unshallow transitions seed parents;
    /// common haves outside candidate history cannot hide newly requested history.
    pub fn select_revision_fetch<'selected>(
        &self,
        roots: &[ObjectId],
        history: (
            impl IntoIterator<Item = Result<ObjectId>>,
            impl IntoIterator<Item = Result<ObjectId>>,
            impl IntoIterator<Item = Result<ObjectId>>,
        ),
        work: &mut [Option<ValidationMetadata>],
        selected: &'selected mut [usize],
        scratch: (&mut [u8], &mut [u8], &mut RevisionState<'_>),
        options: (SelectionOptions, FetchOptions, RevisionOptions),
    ) -> Result<&'selected [usize]> {
        let (read, parse, revision) = scratch;
        revision.count = 0;
        let count = self.objects.len();
        let (selection, fetch, options) = options;
        if roots.is_empty() || fetch.depth.is_some() || options.since == Some(u64::MAX) {
            return Err(Error::InvalidRequest);
        }
        if matches!(fetch.filter, BlobFilter::Type(_) | BlobFilter::TreeDepth(_)) {
            return Err(Error::Unsupported);
        }
        if work.len() < count || selected.len() < count || revision.states.len() < count {
            return Err(Error::BufferTooSmall);
        }
        revision.charge_work((count as u64).checked_mul(3).ok_or(Error::LimitExceeded)?)?;
        let work = &mut work[..count];
        work.copy_from_slice(self.objects);
        revision.states[..count].fill(0);
        for slot in work.iter_mut() {
            let object = slot.as_mut().expect("validated metadata");
            object.reset_walk();
            object.excluded = false;
            object.client_shallow = false;
        }
        let (common, shallow, excluded) = history;
        let mut walk = Walker {
            repository: self,
            objects: work,
            states: &mut revision.states[..count],
            read,
            parse,
            bytes: selection.graph_read_limit,
            work: &mut revision.remaining,
        };
        walk.client_boundaries(shallow)?;
        walk.exclude(excluded, revision.limits.max_exclusions)?;
        walk.candidates(roots, options.since)?;
        walk.exclude_common(common)?;
        walk.wanted_payload(roots)?;
        walk.filter(fetch.filter)?;
        if selection.include_tags {
            walk.include_tags()?;
            walk.objects()?;
            walk.filter(fetch.filter)?;
        }
        let length = walk.selected_indices(selected)?;
        revision.count = count;
        Ok(&selected[..length])
    }
}

enum Parents {
    Pending,
    Boundary,
    Seed,
    Payload(bool),
    Date,
}

struct Walker<'a, 'b, 'index, 'repo, R: Repository + ?Sized> {
    repository: &'a ValidatedRepository<'index, 'repo, R>,
    objects: &'b mut [Option<ValidationMetadata>],
    states: &'b mut [u8],
    read: &'b mut [u8],
    parse: &'b mut [u8],
    bytes: u64,
    work: &'b mut u64,
}

impl<R: Repository + ?Sized> Walker<'_, '_, '_, '_, R> {
    fn client_boundaries(
        &mut self,
        shallow: impl IntoIterator<Item = Result<ObjectId>>,
    ) -> Result<()> {
        for id in shallow {
            let index = self.find(id?).map_err(|error| {
                if error == Error::ObjectNotFound {
                    Error::InvalidRequest
                } else {
                    error
                }
            })?;
            if metadata(self.objects, index).info.kind != ObjectKind::Commit {
                return Err(Error::InvalidRequest);
            }
            self.objects[index]
                .as_mut()
                .expect("validated metadata")
                .client_shallow = true;
        }
        Ok(())
    }

    fn exclude(
        &mut self,
        excluded: impl IntoIterator<Item = Result<ObjectId>>,
        limit: usize,
    ) -> Result<()> {
        for (position, id) in excluded.into_iter().enumerate() {
            charge(self.work, 1)?;
            if position >= limit {
                return Err(Error::LimitExceeded);
            }
            let index = self.find(id?)?;
            self.states[index] |= PENDING;
        }
        self.ancestry(None, EXCLUDED)
    }

    fn candidates(&mut self, roots: &[ObjectId], since: Option<u64>) -> Result<()> {
        for &id in roots {
            let index = self.find(id)?;
            let index = self.peel(index)?;
            if metadata(self.objects, index).info.kind == ObjectKind::Commit {
                self.states[index] |= PENDING;
            }
        }
        self.ancestry(since.filter(|&value| value != 0), CANDIDATE)?;
        charge(self.work, self.states.len() as u64)?;
        if !self.states.iter().any(|state| state & CANDIDATE != 0) {
            return Err(Error::InvalidRequest);
        }
        // Freeze candidate membership before marking borders: a merge border
        // may stop another candidate from appearing in the eventual payload.
        for index in 0..self.states.len() {
            charge(self.work, 1)?;
            if self.states[index] & CANDIDATE != 0 {
                self.commit(index, Parents::Boundary)?;
            }
        }
        for index in 0..self.states.len() {
            charge(self.work, 1)?;
            if metadata(self.objects, index).client_shallow
                && self.states[index] & (CANDIDATE | BORDER) == CANDIDATE
            {
                self.states[index] |= UNSHALLOW;
            }
        }
        Ok(())
    }

    fn exclude_common(&mut self, common: impl IntoIterator<Item = Result<ObjectId>>) -> Result<()> {
        for id in common {
            let index = self.find(id?)?;
            if self.states[index] & CANDIDATE != 0 {
                self.objects[index]
                    .as_mut()
                    .expect("validated metadata")
                    .reached = true;
            }
        }
        self.objects()?;
        for index in 0..self.objects.len() {
            charge(self.work, 1)?;
            let object = self.objects[index].as_mut().expect("validated metadata");
            object.excluded = object.reached || self.states[index] & UNSHALLOW != 0;
            object.reset_walk();
        }
        Ok(())
    }

    fn wanted_payload(&mut self, roots: &[ObjectId]) -> Result<()> {
        for &id in roots {
            let index = self.find(id)?;
            self.objects[index]
                .as_mut()
                .expect("validated metadata")
                .wanted = true;
            charge(self.work, self.lookup_cost())?;
            reach_at(
                self.objects,
                self.repository.by_id,
                id,
                metadata(self.repository.objects, index).info.kind,
                1,
                true,
            )?;
        }
        for index in 0..self.states.len() {
            charge(self.work, 1)?;
            if self.states[index] & UNSHALLOW != 0 {
                self.commit(index, Parents::Seed)?;
            }
        }
        self.objects()
    }

    fn filter(&mut self, filter: BlobFilter) -> Result<()> {
        charge(self.work, self.objects.len() as u64)?;
        filter_objects(self.objects, filter);
        Ok(())
    }

    fn include_tags(&mut self) -> Result<()> {
        charge(
            self.work,
            (self.repository.reference_count() as u64)
                .checked_mul(2 * self.lookup_cost() + 1)
                .ok_or(Error::LimitExceeded)?,
        )?;
        Selection {
            repository: self.repository,
            work: self.objects,
            scratch: (self.read, self.parse),
            remaining: self.bytes,
            filter: BlobFilter::All,
        }
        .include_matching_tags()
    }

    fn selected_indices(&mut self, selected: &mut [usize]) -> Result<usize> {
        charge(self.work, self.objects.len() as u64)?;
        let mut length = 0;
        for (index, object) in self.objects.iter().enumerate() {
            if object.expect("validated metadata").reached {
                selected[length] = index;
                length += 1;
            }
        }
        Ok(length)
    }

    fn lookup_cost(&self) -> u64 {
        lookup_cost(self.objects.len())
    }
    fn find(&mut self, id: ObjectId) -> Result<usize> {
        charge(self.work, self.lookup_cost())?;
        find(self.objects, self.repository.by_id, id).ok_or(Error::ObjectNotFound)
    }
    fn peel(&mut self, mut index: usize) -> Result<usize> {
        for _ in 0..self.objects.len() {
            charge(self.work, 1)?;
            if metadata(self.objects, index).info.kind != ObjectKind::Tag {
                return Ok(index);
            }
            let mut body =
                Body::budgeted(self.repository, index, self.read, Some(&mut self.bytes))?;
            let line = line(&mut body, self.parse, self.work)?;
            let id = link(line, b"object ", self.repository.object_format())?;
            index = self.find(id)?;
        }
        Err(Error::InvalidObject)
    }
    fn ancestry(&mut self, since: Option<u64>, flag: u8) -> Result<()> {
        loop {
            charge(self.work, self.states.len() as u64)?;
            let Some(index) = self
                .states
                .iter()
                .position(|state| state & (PENDING | DONE) == PENDING)
            else {
                break;
            };
            self.states[index] |= DONE;
            if metadata(self.objects, index).info.kind != ObjectKind::Commit {
                let peeled = self.peel(index)?;
                if metadata(self.objects, peeled).info.kind == ObjectKind::Commit {
                    self.states[peeled] |= PENDING;
                }
                continue;
            }
            if flag == CANDIDATE && self.states[index] & EXCLUDED != 0 {
                continue;
            }
            if let Some(cut) = since {
                if self.commit(index, Parents::Date)? < cut {
                    continue;
                }
            }
            self.states[index] |= flag;
            self.commit(index, Parents::Pending)?;
        }
        charge(self.work, self.states.len() as u64)?;
        for state in self.states.iter_mut() {
            *state &= !(PENDING | DONE);
        }
        Ok(())
    }
    fn commit(&mut self, index: usize, action: Parents) -> Result<u64> {
        let lookup = self.lookup_cost();
        let format = self.repository.object_format();
        let mut body = Body::budgeted(self.repository, index, self.read, Some(&mut self.bytes))?;
        let tree = link(line(&mut body, self.parse, self.work)?, b"tree ", format)?;
        if matches!(action, Parents::Payload(_)) {
            charge(self.work, lookup)?;
            reach(self.objects, self.repository.by_id, tree, ObjectKind::Tree)?;
        }
        let mut value = line(&mut body, self.parse, self.work)?;
        while value.starts_with(b"parent ") {
            let id = link(value, b"parent ", format)?;
            if !matches!(action, Parents::Date) {
                charge(self.work, lookup)?;
                let parent =
                    find(self.objects, self.repository.by_id, id).ok_or(Error::ObjectNotFound)?;
                match action {
                    Parents::Pending => self.states[parent] |= PENDING,
                    Parents::Boundary if self.states[parent] & CANDIDATE == 0 => {
                        self.states[index] |= BORDER
                    }
                    Parents::Seed | Parents::Payload(false) => {
                        let object = self.objects[parent].as_mut().expect("validated metadata");
                        if !object.excluded {
                            object.reached = true;
                            object.depth = 1;
                        }
                    }
                    _ => {}
                }
            }
            value = line(&mut body, self.parse, self.work)?;
        }
        if !value.starts_with(b"author ") {
            return Err(Error::InvalidObject);
        }
        let committer = line(&mut body, self.parse, self.work)?;
        if !committer.starts_with(b"committer ") {
            return Err(Error::InvalidObject);
        }
        Ok(commit_date(committer))
    }
    fn objects(&mut self) -> Result<()> {
        loop {
            charge(self.work, self.objects.len() as u64)?;
            let Some(index) = self.objects.iter().position(|slot| {
                let object = slot.expect("validated metadata");
                object.reached && !object.checked
            }) else {
                return Ok(());
            };
            self.objects[index]
                .as_mut()
                .expect("validated metadata")
                .checked = true;
            let object = *metadata(self.objects, index);
            match object.info.kind {
                ObjectKind::Commit => {
                    let stop = object.client_shallow || self.states[index] & BORDER != 0;
                    self.commit(index, Parents::Payload(stop))?;
                }
                ObjectKind::Blob => {}
                kind => {
                    let factor = self.lookup_cost() + 8;
                    let allowance = self.bytes.min(*self.work / factor);
                    let reservation = allowance * factor;
                    charge(self.work, reservation)?;
                    let mut remaining = allowance;
                    let mut body =
                        Body::budgeted(self.repository, index, self.read, Some(&mut remaining))?;
                    let result = if kind == ObjectKind::Tree {
                        validate_tree(
                            &mut body,
                            self.parse,
                            self.objects,
                            self.repository.by_id,
                            object.forced,
                            1,
                        )
                    } else {
                        validate_tag(
                            &mut body,
                            self.parse,
                            self.objects,
                            self.repository.by_id,
                            1,
                            object.forced,
                            object.wanted,
                        )
                    };
                    self.bytes -= allowance - remaining;
                    *self.work += remaining * factor;
                    result?;
                }
            }
        }
    }
}

fn lookup_cost(count: usize) -> u64 {
    u64::from(usize::BITS - count.leading_zeros()) + 1
}
fn charge(remaining: &mut u64, units: u64) -> Result<()> {
    *remaining = remaining.checked_sub(units).ok_or(Error::LimitExceeded)?;
    Ok(())
}
fn line<'a, R: Repository + ?Sized>(
    body: &mut Body<'_, '_, R>,
    output: &'a mut [u8],
    work: &mut u64,
) -> Result<&'a [u8]> {
    let mut length = 0;
    loop {
        charge(work, 1)?;
        let byte = body.byte()?.ok_or(Error::InvalidObject)?;
        if byte == b'\n' {
            return Ok(&output[..length]);
        }
        if byte == 0 {
            return Err(Error::InvalidObject);
        }
        *output.get_mut(length).ok_or(Error::BufferTooSmall)? = byte;
        length += 1;
    }
}

// Git's tolerant historical committer-date extraction is intentionally separate
// from wire-number parsing: decimal prefixes and saturated overflow are accepted.
fn commit_date(line: &[u8]) -> u64 {
    let Some(end) = line.iter().rposition(|&byte| byte == b'>') else {
        return 0;
    };
    let mut digits = &line[end + 1..];
    while digits.first().is_some_and(u8::is_ascii_whitespace) {
        digits = &digits[1..];
    }
    let negative = digits.first() == Some(&b'-');
    if negative {
        digits = &digits[1..];
    }
    let mut value = 0_u64;
    for &byte in digits {
        if !byte.is_ascii_digit() {
            break;
        }
        let Some(next) = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u64::from(byte - b'0')))
        else {
            return u64::MAX;
        };
        value = next;
    }
    if negative {
        value.wrapping_neg()
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn historical_dates_match_git_tolerant_decimal_prefixes() {
        for (date, expected) in [
            ("1700000200 +0000", 1700000200),
            ("1700000200suffix +0000", 1700000200),
            ("1700000200+0100", 1700000200),
            ("-1 +0000", u64::MAX),
            ("-2 +0000", u64::MAX - 1),
            ("+1700000200 +0000", 0),
            ("0x6553f1c8 +0000", 0),
            ("18446744073709551615 +0000", u64::MAX),
            ("18446744073709551616 +0000", u64::MAX),
            ("-18446744073709551616 +0000", u64::MAX),
            ("not-a-date", 0),
            ("  1700000200 +0000", 1700000200),
            ("\t1700000200 +0000", 1700000200),
            ("- +0000", 0),
            ("01700000200 +0000", 1700000200),
        ] {
            let mut line = [0; 256];
            line[0] = b'>';
            line[1..1 + date.len()].copy_from_slice(date.as_bytes());
            assert_eq!(commit_date(&line[..1 + date.len()]), expected, "{date:?}");
        }
    }
}
