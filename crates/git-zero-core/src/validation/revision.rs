//! Revision-list history cuts. Boundary membership is independent of PACK membership.
use super::*;

mod date;
mod state;
use date::commit_date;
use state::{BORDER, CANDIDATE, CLIENT_SHALLOW, DONE, EXCLUDED, PENDING, UNSHALLOW};
pub use state::{RevisionLimits, RevisionOptions, RevisionState, ShallowChange};

impl<R: Repository + ?Sized> ValidatedRepository<'_, '_, R> {
    /// Select revision-limited history with separate caller-owned boundary state.
    /// `history` contains common commits, client boundaries, and resolved exclusion
    /// endpoints. This additive path rejects an absolute depth, preserves blob
    /// filtering/ascending indices, and shares structural bytes across every pass.
    /// Exclusion and candidate passes visit commits only; payload traversal follows
    /// trees/tags with the existing validated graph contract. No blob body is read.
    /// Existing shallow commits are known, and unshallow transitions seed parents;
    /// common haves outside candidate history cannot hide newly requested history.
    /// Metadata scans are bounded by object capacity (quadratic worst case), and
    /// exclusion count is capped independently. Total preparation CPU accounting
    /// is a separate host concern, as with ordinary bounded selection.
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

        let (common, shallow, excluded) = history;
        let mut payload = Selection {
            repository: self,
            work: &mut work[..count],
            scratch: (read, parse),
            remaining: selection.graph_read_limit,
            filter: BlobFilter::All,
        };
        payload.prepare(shallow)?;
        for (state, object) in revision.states[..count].iter_mut().zip(payload.work.iter()) {
            *state = u8::from(object.expect("validated metadata").client_shallow) * CLIENT_SHALLOW;
        }
        let mut walk = Walker {
            repository: self,
            objects: payload.work,
            states: &mut revision.states[..count],
            read: payload.scratch.0,
            parse: payload.scratch.1,
            bytes: payload.remaining,
        };
        walk.exclude(excluded, revision.limits.max_exclusions)?;
        walk.candidates(roots, options.since)?;
        walk.exclude_common(common)?;
        walk.wanted_payload(roots)?;
        filter_objects(walk.objects, fetch.filter);
        if selection.include_tags {
            walk.include_tags()?;
            walk.payload(&[])?;
            filter_objects(walk.objects, fetch.filter);
        }
        let length = selected_indices(walk.objects, selected);
        revision.count = count;
        Ok(&selected[..length])
    }
}

#[derive(Clone, Copy)]
enum Parents {
    Pending,
    Boundary,
    Seed,
}

fn mark_parent(
    action: Parents,
    index: usize,
    parent: usize,
    objects: &mut [Option<ValidationMetadata>],
    states: &mut [u8],
) {
    match action {
        Parents::Pending => states[parent] |= PENDING,
        Parents::Boundary if states[parent] & CANDIDATE == 0 => states[index] |= BORDER,
        Parents::Seed => {
            let object = metadata_mut(objects, parent);
            if !object.excluded {
                object.reached = true;
                object.depth = 1;
            }
        }
        _ => {}
    }
}

struct Walker<'a, 'b, 'index, 'repo, R: Repository + ?Sized> {
    repository: &'a ValidatedRepository<'index, 'repo, R>,
    objects: &'b mut [Option<ValidationMetadata>],
    states: &'b mut [u8],
    read: &'b mut [u8],
    parse: &'b mut [u8],
    bytes: u64,
}

impl<R: Repository + ?Sized> Walker<'_, '_, '_, '_, R> {
    fn exclude(
        &mut self,
        excluded: impl IntoIterator<Item = Result<ObjectId>>,
        limit: usize,
    ) -> Result<()> {
        for (position, id) in excluded.into_iter().enumerate() {
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
            self.queue_commit(index)?;
        }
        self.ancestry(since.filter(|&value| value != 0), CANDIDATE)?;

        if !self.states.iter().any(|state| state & CANDIDATE != 0) {
            return Err(Error::InvalidRequest);
        }
        // Freeze candidate membership before marking borders: a merge border
        // may stop another candidate from appearing in the eventual payload.
        self.visit_parents(CANDIDATE, Parents::Boundary)?;
        state::mark_unshallow(self.states);
        Ok(())
    }

    fn exclude_common(&mut self, common: impl IntoIterator<Item = Result<ObjectId>>) -> Result<()> {
        for id in common {
            let index = self.find(id?)?;
            if self.states[index] & CANDIDATE != 0 {
                metadata_mut(self.objects, index).reached = true;
            }
        }
        self.payload(&[])?;
        for index in 0..self.objects.len() {
            let object = metadata_mut(self.objects, index);
            object.excluded = object.reached || self.states[index] & UNSHALLOW != 0;
            object.reset_walk();
        }
        Ok(())
    }

    fn wanted_payload(&mut self, roots: &[ObjectId]) -> Result<()> {
        self.visit_parents(UNSHALLOW, Parents::Seed)?;
        self.payload(roots)
    }

    fn include_tags(&mut self) -> Result<()> {
        Selection {
            repository: self.repository,
            work: self.objects,
            scratch: (self.read, self.parse),
            remaining: self.bytes,
            filter: BlobFilter::All,
        }
        .include_matching_tags()
    }

    fn find(&self, id: ObjectId) -> Result<usize> {
        find(self.objects, self.repository.by_id, id).ok_or(Error::ObjectNotFound)
    }
    fn peel(&mut self, mut index: usize) -> Result<usize> {
        for _ in 0..self.objects.len() {
            if metadata(self.objects, index).info.kind != ObjectKind::Tag {
                return Ok(index);
            }
            let mut body =
                Body::budgeted(self.repository, index, self.read, Some(&mut self.bytes))?;
            let line = body.line(self.parse)?;
            let id = link(line, b"object ", self.repository.object_format())?;
            index = self.find(id)?;
        }
        Err(Error::InvalidObject)
    }
    // Exclusions and explicit wants both peel annotated endpoints before queuing.
    fn queue_commit(&mut self, index: usize) -> Result<()> {
        let index = self.peel(index)?;
        if metadata(self.objects, index).info.kind == ObjectKind::Commit {
            self.states[index] |= PENDING;
        }
        Ok(())
    }
    fn ancestry(&mut self, since: Option<u64>, flag: u8) -> Result<()> {
        while let Some(index) = self
            .states
            .iter()
            .position(|state| state & (PENDING | DONE) == PENDING)
        {
            self.states[index] |= DONE;
            self.visit_ancestor(index, since, flag)?;
        }

        for state in self.states.iter_mut() {
            *state &= !(PENDING | DONE);
        }
        Ok(())
    }
    fn visit_ancestor(&mut self, index: usize, since: Option<u64>, flag: u8) -> Result<()> {
        if metadata(self.objects, index).info.kind != ObjectKind::Commit {
            return self.queue_commit(index);
        }
        if flag == CANDIDATE && self.states[index] & EXCLUDED != 0 {
            return Ok(());
        }
        if let Some(cut) = since {
            if self.date(index)? < cut {
                return Ok(());
            }
        }
        self.states[index] |= flag;
        self.commit(index, Parents::Pending)
    }
    fn date(&mut self, index: usize) -> Result<u64> {
        let mut body = Body::budgeted(self.repository, index, self.read, Some(&mut self.bytes))?;
        commit_tree(&mut body, self.parse)?;
        let committer = commit_parents(&mut body, self.parse, |_| Ok(()))?;
        Ok(commit_date(committer))
    }

    // Both parent passes use stable provider order with their own state mask
    // and parent transition.
    fn visit_parents(&mut self, flag: u8, action: Parents) -> Result<()> {
        for index in 0..self.states.len() {
            if self.states[index] & flag != 0 {
                self.commit(index, action)?;
            }
        }
        Ok(())
    }

    fn commit(&mut self, index: usize, action: Parents) -> Result<()> {
        let mut body = Body::budgeted(self.repository, index, self.read, Some(&mut self.bytes))?;
        commit_tree(&mut body, self.parse)?;
        commit_parents(&mut body, self.parse, |id| {
            let parent =
                find(self.objects, self.repository.by_id, id).ok_or(Error::ObjectNotFound)?;
            mark_parent(action, index, parent, self.objects, self.states);
            Ok(())
        })?;
        Ok(())
    }
    fn payload(&mut self, roots: &[ObjectId]) -> Result<()> {
        // Revision borders temporarily act as client stops for the shared
        // wanted-object walker. Caller markers are restored on success.
        self.payload_boundaries(false);
        let mut payload = Selection {
            repository: self.repository,
            work: self.objects,
            scratch: (self.read, self.parse),
            remaining: self.bytes,
            filter: BlobFilter::All,
        };
        let result = payload.walk_wants(
            roots,
            History {
                client: true,
                ..History::default()
            },
        );
        self.bytes = payload.remaining;
        result?;
        self.payload_boundaries(true);
        Ok(())
    }

    fn payload_boundaries(&mut self, restore: bool) {
        for (object, &state) in self.objects.iter_mut().zip(self.states.iter()) {
            object.as_mut().expect("validated metadata").client_shallow =
                state & CLIENT_SHALLOW != 0 || (!restore && state & BORDER != 0);
        }
    }
}
