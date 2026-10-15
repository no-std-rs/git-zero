#[path = "common/allocations.rs"]
mod allocations;

use git_zero::{
    Error, Head, IndexedRepository, Object, ObjectId, ObjectInfo, ObjectKind, RefRepository,
    Reference, Repository, Result, ValidatedRepository,
    cache::{self, Key, PackCache},
    http::{self, Request, SelectionStorage},
    io::SliceWriter,
    pack, pktline,
    repository::{MAX_PEELED_REF_LENGTH, resolve_reference},
};
use std::{cell::Cell, io::Write as _};

const ZERO: ObjectId = ObjectId::Sha1([0; 20]);
const CAPACITY: usize = 32;

#[derive(Clone)]
struct Owned {
    kind: ObjectKind,
    data: Vec<u8>,
    id: ObjectId,
}
impl Owned {
    fn new(kind: ObjectKind, data: Vec<u8>) -> Self {
        let id = Object { kind, data: &data }.id();
        Self { kind, data, id }
    }
    fn borrowed(&self) -> Object<'_> {
        Object {
            kind: self.kind,
            data: &self.data,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Ids {
    base_blob: Option<ObjectId>,
    main_blob: Option<ObjectId>,
    side_blob: Option<ObjectId>,
    shared: Option<ObjectId>,
    base_tree: Option<ObjectId>,
    main_tree: Option<ObjectId>,
    side_tree: Option<ObjectId>,
    merge_tree: Option<ObjectId>,
    base: Option<ObjectId>,
    main: Option<ObjectId>,
    side: Option<ObjectId>,
    merge: Option<ObjectId>,
    tag: Option<ObjectId>,
    nested: Option<ObjectId>,
}

#[derive(Clone)]
struct Graph {
    objects: Vec<Owned>,
    refs: Vec<Reference<'static>>,
    ids: Ids,
}
impl Repository for Graph {
    fn head(&self) -> ObjectId {
        self.ids.main.unwrap_or(ZERO)
    }
    fn object_count(&self) -> usize {
        self.objects.len()
    }
    fn object(&self, index: usize) -> Option<Object<'_>> {
        self.objects.get(index).map(Owned::borrowed)
    }
    fn object_id(&self, index: usize) -> Option<ObjectId> {
        self.objects.get(index).map(|object| object.id)
    }
    // Validation and selection must use their own index, not a provider lookup.
    fn find_index(&self, _id: ObjectId) -> Option<usize> {
        None
    }
}

fn add(objects: &mut Vec<Owned>, kind: ObjectKind, data: Vec<u8>) -> ObjectId {
    let object = Owned::new(kind, data);
    let id = object.id;
    objects.push(object);
    id
}
fn tree(objects: &mut Vec<Owned>, entries: &[(&str, ObjectId)]) -> ObjectId {
    let mut bytes = Vec::new();
    for &(name, id) in entries {
        bytes.extend_from_slice(b"100644 ");
        bytes.extend_from_slice(name.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(id.as_bytes());
    }
    add(objects, ObjectKind::Tree, bytes)
}
fn commit(
    objects: &mut Vec<Owned>,
    tree: ObjectId,
    parents: &[ObjectId],
    message: &str,
) -> ObjectId {
    let mut bytes = format!("tree {tree}\n").into_bytes();
    for parent in parents {
        writeln!(bytes, "parent {parent}").unwrap();
    }
    bytes.extend_from_slice(
        b"author A <a@example.invalid> 1 +0000\ncommitter A <a@example.invalid> 1 +0000\n\n",
    );
    bytes.extend_from_slice(message.as_bytes());
    add(objects, ObjectKind::Commit, bytes)
}
fn annotation(objects: &mut Vec<Owned>, target: ObjectId, kind: &str, name: &str) -> ObjectId {
    add(objects, ObjectKind::Tag, format!("object {target}\ntype {kind}\ntag {name}\ntagger A <a@example.invalid> 1 +0000\n\nannotation\n").into_bytes())
}

impl Graph {
    fn new() -> Self {
        let mut objects = Vec::new();
        let base_blob = add(&mut objects, ObjectKind::Blob, b"base\0\xff".to_vec());
        let main_blob = add(&mut objects, ObjectKind::Blob, b"main\n".to_vec());
        let side_blob = add(&mut objects, ObjectKind::Blob, b"side\n".to_vec());
        let shared = add(&mut objects, ObjectKind::Blob, b"shared\n".to_vec());
        let base_tree = tree(&mut objects, &[("file", base_blob), ("shared", shared)]);
        let main_tree = tree(&mut objects, &[("file", main_blob), ("shared", shared)]);
        let side_tree = tree(&mut objects, &[("file", side_blob), ("shared", shared)]);
        let merge_tree = tree(
            &mut objects,
            &[("main", main_blob), ("shared", shared), ("side", side_blob)],
        );
        let base = commit(&mut objects, base_tree, &[], "base\n");
        let main = commit(&mut objects, main_tree, &[base], "main\n");
        let side = commit(&mut objects, side_tree, &[base], "side\n");
        let merge = commit(&mut objects, merge_tree, &[main, side], "merge\n");
        let tag = annotation(&mut objects, side, "commit", "annotated");
        let nested = annotation(&mut objects, tag, "tag", "nested");
        let refs = vec![
            Reference::direct("refs/heads/a-side", side),
            Reference {
                name: "refs/heads/alias",
                id: main,
                symbolic_target: Some("refs/heads/alias-next"),
                peeled: None,
            },
            Reference {
                name: "refs/heads/alias-next",
                id: main,
                symbolic_target: Some("refs/heads/z-main"),
                peeled: None,
            },
            Reference::direct("refs/heads/z-main", main),
            Reference::direct("refs/heads/z-merge", merge),
            Reference {
                name: "refs/tags/annotated",
                id: tag,
                symbolic_target: None,
                peeled: Some(side),
            },
            Reference::direct("refs/tags/blob", shared),
            Reference::direct("refs/tags/light", base),
            Reference {
                name: "refs/tags/nested",
                id: nested,
                symbolic_target: None,
                peeled: Some(side),
            },
            Reference::direct("refs/tags/tree", base_tree),
        ];
        Self {
            objects,
            refs,
            ids: Ids {
                base_blob: Some(base_blob),
                main_blob: Some(main_blob),
                side_blob: Some(side_blob),
                shared: Some(shared),
                base_tree: Some(base_tree),
                main_tree: Some(main_tree),
                side_tree: Some(side_tree),
                merge_tree: Some(merge_tree),
                base: Some(base),
                main: Some(main),
                side: Some(side),
                merge: Some(merge),
                tag: Some(tag),
                nested: Some(nested),
            },
        }
    }
    fn state(&self) -> Head<'static> {
        Head::Symbolic {
            target: "refs/heads/alias",
            id: self.head(),
        }
    }
    fn repository(&self) -> RefRepository<'_, '_, Self> {
        RefRepository::new(
            self,
            Head::Symbolic {
                target: "refs/heads/alias",
                id: self.head(),
            },
            &self.refs,
        )
        .unwrap()
    }
    fn main_closure(&self) -> Vec<ObjectId> {
        [
            self.ids.base_blob,
            self.ids.main_blob,
            self.ids.shared,
            self.ids.base_tree,
            self.ids.main_tree,
            self.ids.base,
            self.ids.main,
        ]
        .map(Option::unwrap)
        .to_vec()
    }
    fn side_closure(&self) -> Vec<ObjectId> {
        [
            self.ids.base_blob,
            self.ids.side_blob,
            self.ids.shared,
            self.ids.base_tree,
            self.ids.side_tree,
            self.ids.base,
            self.ids.side,
        ]
        .map(Option::unwrap)
        .to_vec()
    }
    fn merge_closure(&self) -> Vec<ObjectId> {
        [
            self.ids.base_blob,
            self.ids.main_blob,
            self.ids.side_blob,
            self.ids.shared,
            self.ids.base_tree,
            self.ids.main_tree,
            self.ids.side_tree,
            self.ids.merge_tree,
            self.ids.base,
            self.ids.main,
            self.ids.side,
            self.ids.merge,
        ]
        .map(Option::unwrap)
        .to_vec()
    }
}

fn validated<R: Repository, T>(
    repository: &R,
    operation: impl FnOnce(&ValidatedRepository<'_, '_, R>) -> T,
) -> T {
    let mut metadata = [None; CAPACITY];
    let mut by_id = [usize::MAX; CAPACITY];
    let validated = allocations::without_allocations(|| {
        ValidatedRepository::new(
            repository,
            &mut metadata,
            &mut by_id,
            &mut [0; 1],
            &mut [0; 128],
        )
        .unwrap()
    });
    operation(&validated)
}
fn validation_result(repository: &impl Repository) -> Result<()> {
    ValidatedRepository::new(
        repository,
        &mut [None; CAPACITY],
        &mut [0; CAPACITY],
        &mut [0; 1],
        &mut [0; 128],
    )
    .map(|_| ())
}
fn sorted(mut ids: Vec<ObjectId>) -> Vec<ObjectId> {
    ids.sort_unstable();
    ids.dedup();
    ids
}
fn selected_ids(repository: &impl Repository, indices: &[usize]) -> Vec<ObjectId> {
    sorted(
        indices
            .iter()
            .map(|&index| repository.object_id(index).unwrap())
            .collect(),
    )
}

fn check_missing_closure(
    graph: &Graph,
    repository: &ValidatedRepository<'_, '_, impl Repository>,
    common: &[ObjectId],
    work: &mut [Option<git_zero::ValidationMetadata>],
    indices: &mut [usize],
) {
    let excluded = if common.is_empty() {
        vec![]
    } else {
        let selected = repository
            .select_objects(common, work, indices, &mut [0; 1], &mut [0; 128], 4096)
            .unwrap();
        selected_ids(repository, selected)
    };
    let selected = allocations::without_allocations(|| {
        repository
            .select_missing_objects(
                &[graph.ids.merge.unwrap()],
                common.iter().copied().map(Ok),
                work,
                indices,
                (&mut [0; 1], &mut [0; 128]),
                8192,
            )
            .unwrap()
    });
    let expected = graph
        .objects
        .iter()
        .filter(|object| object.kind != ObjectKind::Tag && !excluded.contains(&object.id))
        .map(|object| object.id)
        .collect();
    assert_eq!(selected_ids(repository, selected), sorted(expected));
}

#[test]
fn common_commit_closures_exclude_shared_merge_history_and_reset_between_requests() {
    let graph = Graph::new();
    validated(&graph.repository(), |repository| {
        let mut work = [None; CAPACITY];
        let mut indices = [0; CAPACITY];
        assert_eq!(
            repository.select_missing_objects(
                &[graph.ids.merge.unwrap()],
                [Err(Error::Backend)],
                &mut work,
                &mut indices,
                (&mut [0; 1], &mut [0; 128]),
                8192,
            ),
            Err(Error::Backend)
        );
        for common in [
            vec![graph.ids.base.unwrap()],
            vec![graph.ids.main.unwrap(), graph.ids.side.unwrap()],
            vec![graph.ids.side.unwrap(), graph.ids.side.unwrap()],
            vec![],
        ] {
            check_missing_closure(&graph, repository, &common, &mut work, &mut indices);
        }
    });
}

fn request(roots: &[ObjectId], done: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    for id in roots {
        writeln!(bytes, "0032want {id}").unwrap();
    }
    bytes.extend_from_slice(b"0000");
    if done {
        bytes.extend_from_slice(b"0009done\n");
    }
    bytes
}

#[test]
fn distinct_tips_symbolic_chains_and_peeled_refs_survive_indexes() {
    let graph = Graph::new();
    let repository = graph.repository();
    assert_eq!(
        resolve_reference(&repository, "refs/heads/alias"),
        Ok(("refs/heads/z-main", graph.head()))
    );
    let mut objects = [None; CAPACITY];
    let mut by_id = [0; CAPACITY];
    let indexed = IndexedRepository::new(&repository, &mut objects, &mut by_id).unwrap();
    validated(&indexed, |repo| {
        for (index, reference) in graph.refs.iter().enumerate() {
            assert_eq!(repo.reference_name(index), Some(reference.name));
            assert_eq!(repo.reference_id(index), Some(reference.id));
            assert_eq!(
                repo.reference_symbolic_target(index),
                reference.symbolic_target
            );
            assert_eq!(repo.reference_peeled(index), reference.peeled);
        }
        assert_eq!(repo.object_count(), graph.objects.len());
    });
}

#[test]
fn symbolic_head_can_resolve_through_a_lightweight_tag() {
    let mut source = Graph::new();
    source.refs[2].symbolic_target = Some("refs/tags/light");
    source.refs[7].id = source.head();
    let repository = allocations::without_allocations(|| {
        RefRepository::new(&source, source.state(), &source.refs).unwrap()
    });
    validated(&repository, |repository| {
        assert_eq!(
            resolve_reference(repository, "refs/heads/alias"),
            Ok(("refs/tags/light", source.head()))
        );
        let mut storage = [0; 4096];
        let mut output = SliceWriter::new(&mut storage);
        allocations::without_allocations(|| {
            http::serve(
                repository,
                Request {
                    content_type: None,
                    git_protocol: None,
                    method: "GET",
                    path: "info/refs",
                    query: Some("service=git-upload-pack"),
                    body: &[],
                },
                &mut output,
            )
            .unwrap()
        });
        assert!(
            output
                .as_bytes()
                .windows(b"symref=HEAD:refs/tags/light".len())
                .any(|part| part == b"symref=HEAD:refs/tags/light")
        );
    });
}

fn assert_selected_objects(
    repo: &ValidatedRepository<'_, '_, impl Repository>,
    (roots, expected): (&[ObjectId], Vec<ObjectId>),
    work: &mut [Option<git_zero::ValidationMetadata>],
    indices: &mut [usize],
) {
    let selected = allocations::without_allocations(|| {
        repo.select_objects(roots, work, indices, &mut [0; 1], &mut [0; 128], 4096)
            .unwrap()
    });
    assert_eq!(selected_ids(repo, selected), sorted(expected));
    assert!(work[repo.object_count()..].iter().all(Option::is_none));
    assert!(
        indices[repo.object_count()..]
            .iter()
            .all(|&index| index == usize::MAX)
    );
}

#[test]
fn selected_closures_deduplicate_roots_and_reset_state_without_allocating() {
    let graph = Graph::new();
    validated(&graph.repository(), |repo| {
        let mut work = [None; CAPACITY];
        let mut indices = [usize::MAX; CAPACITY];
        for (roots, expected) in [
            (vec![graph.head()], graph.main_closure()),
            (vec![graph.ids.side.unwrap()], graph.side_closure()),
            (vec![graph.ids.merge.unwrap()], graph.merge_closure()),
            (vec![graph.ids.nested.unwrap()], {
                let mut ids = graph.side_closure();
                ids.extend([graph.ids.tag.unwrap(), graph.ids.nested.unwrap()]);
                ids
            }),
            (
                vec![graph.ids.merge.unwrap(), graph.ids.nested.unwrap()],
                graph.objects.iter().map(|object| object.id).collect(),
            ),
            (vec![graph.head(), graph.head()], graph.main_closure()),
            (
                vec![graph.ids.shared.unwrap()],
                vec![graph.ids.shared.unwrap()],
            ),
            (vec![graph.head()], graph.main_closure()),
        ] {
            assert_selected_objects(repo, (&roots, expected), &mut work, &mut indices);
        }
        let mut other_work = [None; CAPACITY];
        let mut other_indices = [0; CAPACITY];
        let main = repo
            .select_objects(
                &[graph.head()],
                &mut work,
                &mut indices,
                &mut [0; 1],
                &mut [0; 128],
                4096,
            )
            .unwrap();
        let side = repo
            .select_objects(
                &[graph.ids.side.unwrap()],
                &mut other_work,
                &mut other_indices,
                &mut [0; 1],
                &mut [0; 128],
                4096,
            )
            .unwrap();
        assert_eq!(selected_ids(repo, main), sorted(graph.main_closure()));
        assert_eq!(selected_ids(repo, side), sorted(graph.side_closure()));
    });
}

#[test]
fn invalid_symbolic_refs_and_peeling_fail_before_serving() {
    let graph = Graph::new();
    for target in ["refs/heads/missing", "refs/heads/alias", "not-a-ref"] {
        let mut refs = graph.refs.clone();
        refs[1].symbolic_target = Some(target);
        assert!(matches!(
            RefRepository::new(&graph, graph.state(), &refs),
            Err(Error::InvalidReference)
        ));
    }
    let mut refs = graph.refs.clone();
    refs[1].id = graph.ids.side.unwrap();
    assert!(matches!(
        RefRepository::new(&graph, graph.state(), &refs),
        Err(Error::InvalidReference)
    ));
    for peeled in [None, Some(graph.head()), Some(graph.ids.tag.unwrap())] {
        let mut refs = graph.refs.clone();
        refs[5].peeled = peeled;
        let repository = RefRepository::new(&graph, graph.state(), &refs).unwrap();
        assert_eq!(validation_result(&repository), Err(Error::InvalidObject));
    }
    let mut refs = graph.refs.clone();
    refs[7].peeled = Some(graph.ids.base.unwrap());
    assert_eq!(
        validation_result(&RefRepository::new(&graph, graph.state(), &refs).unwrap()),
        Err(Error::InvalidObject)
    );
    let mut refs = graph.refs.clone();
    refs[0].id = graph.ids.tag.unwrap();
    assert_eq!(
        validation_result(&RefRepository::new(&graph, graph.state(), &refs).unwrap()),
        Err(Error::InvalidObject)
    );
}

fn tag_result(blob: &Owned, data: Vec<u8>) -> Result<()> {
    let tag = Owned::new(ObjectKind::Tag, data);
    let source = Graph {
        objects: vec![blob.clone(), tag.clone()],
        refs: vec![],
        ids: Ids::default(),
    };
    let references = [Reference {
        name: "refs/tags/test",
        id: tag.id,
        symbolic_target: None,
        peeled: Some(blob.id),
    }];
    validation_result(&RefRepository::new(
        &source,
        Head::Unborn("refs/heads/main"),
        &references,
    )?)
}

#[test]
fn tag_links_headers_and_canonical_ids_are_validated() {
    let blob = Owned::new(ObjectKind::Blob, b"payload".to_vec());
    let valid = format!(
        "object {}\ntype blob\ntag test\ntagger A <a@example.invalid> 1 +0000\n\nmessage",
        blob.id
    );
    assert_eq!(tag_result(&blob, valid.as_bytes().to_vec()), Ok(()));
    for data in [
        valid.replace("type blob", "type commit"),
        valid.replace("type blob", "type unknown"),
        valid.replace("tag test\n", ""),
        valid.replace("tag test", "tag "),
        valid.replace("tag test", "tag\0test"),
        valid.replace("tagger A", "object A"),
    ] {
        assert_eq!(
            tag_result(&blob, data.into_bytes()),
            Err(Error::InvalidObject)
        );
    }
    assert_eq!(
        tag_result(
            &blob,
            valid
                .replace(&blob.id.to_string(), &ZERO.to_string())
                .into_bytes()
        ),
        Err(Error::InvalidObject)
    );
    assert_eq!(
        tag_result(
            &blob,
            valid
                .replace(&blob.id.to_string(), &ObjectId::Sha1([9; 20]).to_string())
                .into_bytes()
        ),
        Err(Error::ObjectNotFound)
    );
    let mut graph = Graph::new();
    graph.objects.last_mut().unwrap().data.push(0);
    assert_eq!(
        validation_result(&graph.repository()),
        Err(Error::InvalidObject)
    );
    let mut graph = Graph::new();
    add(&mut graph.objects, ObjectKind::Blob, b"orphan".to_vec());
    assert_eq!(
        validation_result(&graph.repository()),
        Err(Error::InvalidObject)
    );
}

#[test]
fn unborn_and_detached_heads_can_have_independent_ref_roots() {
    let graph = Graph::new();
    for head in [
        Head::Unborn("refs/heads/new"),
        Head::Detached(graph.ids.base.unwrap()),
    ] {
        let repository = RefRepository::new(&graph, head, &graph.refs).unwrap();
        validated(&repository, |repo| {
            assert_eq!(repo.head_state(), head);
            let mut bytes = [0; 4096];
            let mut output = SliceWriter::new(&mut bytes);
            http::serve(
                repo,
                Request {
                    content_type: None,
                    git_protocol: None,
                    method: "GET",
                    path: "info/refs",
                    query: Some("service=git-upload-pack"),
                    body: &[],
                },
                &mut output,
            )
            .unwrap();
            let wire = output.as_bytes();
            assert!(!wire.windows(7).any(|part| part == b"symref="));
            if head.id().is_none() {
                assert!(!wire.windows(15).any(|part| part == b"capabilities^{}"));
                assert!(!wire.windows(5).any(|part| part == b" HEAD"));
            }
        });
    }
}

#[test]
fn smart_and_dumb_ref_routes_preserve_symbolic_and_peeled_data() {
    let graph = Graph::new();
    validated(&graph.repository(), |repo| {
        for (path, expected) in [
            ("HEAD", b"ref: refs/heads/alias\n".to_vec()),
            ("refs/heads/alias", b"ref: refs/heads/alias-next\n".to_vec()),
            (
                "refs/heads/a-side",
                format!("{}\n", graph.ids.side.unwrap()).into_bytes(),
            ),
        ] {
            let mut bytes = [0; 4096];
            let mut output = SliceWriter::new(&mut bytes);
            http::serve(
                repo,
                Request {
                    content_type: None,
                    git_protocol: None,
                    method: "GET",
                    path,
                    query: None,
                    body: &[],
                },
                &mut output,
            )
            .unwrap();
            assert_eq!(output.as_bytes(), expected);
        }
        for smart in [false, true] {
            let mut bytes = [0; 4096];
            let mut output = SliceWriter::new(&mut bytes);
            http::serve(
                repo,
                Request {
                    content_type: None,
                    git_protocol: None,
                    method: "GET",
                    path: "info/refs",
                    query: smart.then_some("service=git-upload-pack"),
                    body: &[],
                },
                &mut output,
            )
            .unwrap();
            let wire = output.as_bytes();
            let expected = format!(
                "{}{}refs/tags/nested^{{}}",
                graph.ids.side.unwrap(),
                if smart { " " } else { "\t" }
            );
            assert!(
                wire.windows(expected.len())
                    .any(|part| part == expected.as_bytes())
            );
            if smart {
                assert!(
                    wire.windows(b"symref=HEAD:refs/heads/z-main".len())
                        .any(|part| part == b"symref=HEAD:refs/heads/z-main")
                );
            }
        }
    });
}

#[test]
fn peeled_ref_packet_budgets_include_the_suffix() {
    let graph = Graph::new();
    let name = format!("refs/tags/{}", "z".repeat(MAX_PEELED_REF_LENGTH - 10));
    let mut refs: Vec<Reference<'_>> = graph.refs.clone();
    refs[5].name = &name;
    refs.sort_unstable_by_key(|reference| reference.name);
    let repository = RefRepository::new(&graph, graph.state(), &refs).unwrap();
    let mut bytes = vec![0; 160_000];
    let mut output = SliceWriter::new(&mut bytes);
    http::serve(
        &repository,
        Request {
            content_type: None,
            git_protocol: None,
            method: "GET",
            path: "info/refs",
            query: Some("service=git-upload-pack"),
            body: &[],
        },
        &mut output,
    )
    .unwrap();
    let mut wire = output.as_bytes();
    while pktline::read(&mut wire).unwrap().is_some() {}
    let oversized = format!("{name}z");
    let mut refs: Vec<Reference<'_>> = graph.refs.clone();
    refs[5].name = &oversized;
    refs.sort_unstable_by_key(|reference| reference.name);
    assert!(matches!(
        RefRepository::new(&graph, graph.state(), &refs),
        Err(Error::InvalidReference)
    ));
}

#[test]
fn selection_budgets_and_missing_roots_preserve_retryability() {
    let graph = Graph::new();
    validated(&graph.repository(), |repo| {
        let mut work = [None; CAPACITY];
        let mut indices = [0; CAPACITY];
        let count = repo.object_count();
        assert_eq!(
            repo.select_objects(
                &[],
                &mut work,
                &mut indices,
                &mut [0; 1],
                &mut [0; 128],
                4096
            ),
            Err(Error::InvalidRequest)
        );
        assert_eq!(
            repo.select_objects(
                &[ZERO],
                &mut work,
                &mut indices,
                &mut [0; 1],
                &mut [0; 128],
                4096
            ),
            Err(Error::ObjectNotFound)
        );
        assert_eq!(
            repo.select_objects(
                &[graph.head()],
                &mut work[..count - 1],
                &mut indices,
                &mut [0; 1],
                &mut [0; 128],
                4096
            ),
            Err(Error::BufferTooSmall)
        );
        assert_eq!(
            repo.select_objects(
                &[graph.head()],
                &mut work,
                &mut indices[..count - 1],
                &mut [0; 1],
                &mut [0; 128],
                4096
            ),
            Err(Error::BufferTooSmall)
        );
        assert_eq!(
            repo.select_objects(
                &[graph.head()],
                &mut work,
                &mut indices,
                &mut [],
                &mut [0; 128],
                4096
            ),
            Err(Error::BufferTooSmall)
        );
        assert_eq!(
            repo.select_objects(
                &[graph.head()],
                &mut work,
                &mut indices,
                &mut [0; 1],
                &mut [],
                4096
            ),
            Err(Error::BufferTooSmall)
        );
        let selected = repo
            .select_objects(
                &[graph.ids.side.unwrap()],
                &mut work,
                &mut indices,
                &mut [0; 1],
                &mut [0; 128],
                4096,
            )
            .unwrap();
        assert_eq!(selected_ids(repo, selected), sorted(graph.side_closure()));
    });
}

fn serve_selected<R: Repository>(
    repository: &ValidatedRepository<'_, '_, R>,
    roots: &[ObjectId],
    want_budget: usize,
    object_budget: usize,
    done: bool,
) -> (Result<http::Response>, Vec<u8>) {
    let request = request(roots, done);
    let mut wants = [ZERO; 4];
    let mut work = [None; CAPACITY];
    let mut indices = [0; CAPACITY];
    let mut storage = SelectionStorage {
        wants: &mut wants[..want_budget],
        objects: &mut work[..object_budget],
        indices: &mut indices[..object_budget],
        read: &mut [0; 1],
        parse: &mut [0; 128],
        graph_read_limit: 4096,
    };
    let mut bytes = [0; 8192];
    let mut output = SliceWriter::new(&mut bytes);
    let result = allocations::without_allocations(|| {
        http::serve_with_selection(
            repository,
            Request {
                content_type: Some("application/x-git-upload-pack-request"),
                git_protocol: None,
                method: "POST",
                path: "git-upload-pack",
                query: None,
                body: &request,
            },
            &mut storage,
            &mut output,
        )
    });
    (result, output.as_bytes().to_vec())
}

#[test]
fn upload_pack_enforces_advertised_wants_and_preflights_all_selection_budgets() {
    let graph = Graph::new();
    validated(&graph.repository(), |repo| {
        for id in [ZERO, ObjectId::Sha1([9; 20]), graph.ids.main_blob.unwrap()] {
            let (result, bytes) = serve_selected(repo, &[id], 4, CAPACITY, true);
            assert!(matches!(result, Err(Error::InvalidRequest)));
            assert!(bytes.is_empty());
        }
        for (roots, want_budget, object_budget) in [
            (vec![graph.head()], 0, CAPACITY),
            (vec![graph.head(), graph.ids.side.unwrap()], 1, CAPACITY),
            (vec![graph.head()], 1, repo.object_count() - 1),
        ] {
            let (result, bytes) = serve_selected(repo, &roots, want_budget, object_budget, true);
            assert!(matches!(result, Err(Error::BufferTooSmall)));
            assert!(bytes.is_empty());
        }
        let (result, bytes) =
            serve_selected(repo, &[graph.head(), graph.head()], 1, CAPACITY, true);
        assert_eq!(result.unwrap().status, 200);
        pack::validate(&bytes[8..]).unwrap();
        assert_eq!(
            u32::from_be_bytes(bytes[16..20].try_into().unwrap()) as usize,
            graph.main_closure().len()
        );
        let (result, bytes) = serve_selected(repo, &[graph.head()], 1, 0, false);
        assert_eq!(result.unwrap().status, 200);
        assert_eq!(bytes, b"0008NAK\n");
        let (result, bytes) = serve_selected(
            repo,
            &[graph.ids.nested.unwrap(), graph.ids.merge.unwrap()],
            2,
            CAPACITY,
            true,
        );
        assert_eq!(result.unwrap().status, 200);
        pack::validate(&bytes[8..]).unwrap();
        assert_eq!(
            u32::from_be_bytes(bytes[16..20].try_into().unwrap()) as usize,
            graph.objects.len()
        );
    });
}

struct UnusedCache;
impl PackCache for UnusedCache {
    fn get(&self, _key: Key) -> Result<Option<&[u8]>> {
        panic!("unsafe cache key lookup")
    }
    fn put(&mut self, _key: Key, _pack: &[u8]) -> Result<()> {
        panic!("unsafe cache insertion")
    }
}

#[test]
fn whole_repository_pack_hooks_and_head_keys_reject_independent_roots() {
    let graph = Graph::new();
    let repository = graph.repository();
    let body = request(&[graph.head()], true);
    let mut bytes = [0; 4096];
    let mut output = SliceWriter::new(&mut bytes);
    let mut called = false;
    assert!(matches!(
        http::serve_with_pack(
            &repository,
            Request {
                content_type: Some("application/x-git-upload-pack-request"),
                git_protocol: None,
                method: "POST",
                path: "git-upload-pack",
                query: None,
                body: &body
            },
            &mut [],
            &mut output,
            |_, _| {
                called = true;
                Ok(())
            }
        ),
        Err(Error::Unsupported)
    ));
    assert!(!called);
    assert!(output.as_bytes().is_empty());
    assert_eq!(
        cache::write(&repository, &mut UnusedCache, &mut [], &mut output),
        Err(Error::Unsupported)
    );
    assert!(output.as_bytes().is_empty());
    assert_eq!(
        pack::write_selected_with_scratch(&repository, &[0, 0], &mut [], &mut output),
        Err(Error::InvalidObject)
    );
    assert_eq!(
        pack::write_selected_with_scratch(&repository, &[usize::MAX], &mut [], &mut output),
        Err(Error::ObjectNotFound)
    );
    assert!(output.as_bytes().is_empty());
}

struct Streaming<'a> {
    source: &'a Graph,
    fail: Cell<bool>,
    bytes: Cell<usize>,
}

#[test]
fn graph_read_limits_bound_actual_io_and_fail_before_pack_output() {
    let graph = Graph::new();
    let source = Streaming {
        source: &graph,
        fail: Cell::new(false),
        bytes: Cell::new(0),
    };
    let repository = RefRepository::new(&source, graph.state(), &graph.refs).unwrap();
    validated(&repository, |repo| {
        let mut work = [None; CAPACITY];
        let mut indices = [0; CAPACITY];
        let mut read = [0; 128];
        for capacity in [1, 17, 128] {
            source.bytes.set(0);
            assert_eq!(
                repo.select_objects(
                    &[graph.head()],
                    &mut work,
                    &mut indices,
                    &mut read[..capacity],
                    &mut [0; 128],
                    16
                ),
                Err(Error::LimitExceeded)
            );
            assert_eq!(source.bytes.get(), 16);
        }
        source.bytes.set(0);
        let body = request(&[graph.head()], true);
        let mut wants = [ZERO; 1];
        let mut parse = [0; 128];
        let mut storage = SelectionStorage {
            wants: &mut wants,
            objects: &mut work,
            indices: &mut indices,
            read: &mut read,
            parse: &mut parse,
            graph_read_limit: 0,
        };
        let mut bytes = [0; 8192];
        let mut output = SliceWriter::new(&mut bytes);
        assert!(matches!(
            http::serve_with_selection(
                repo,
                Request {
                    content_type: Some("application/x-git-upload-pack-request"),
                    git_protocol: None,
                    method: "POST",
                    path: "git-upload-pack",
                    query: None,
                    body: &body
                },
                &mut storage,
                &mut output
            ),
            Err(Error::LimitExceeded)
        ));
        assert!(output.as_bytes().is_empty());
        assert_eq!(source.bytes.get(), 0);
        storage.graph_read_limit = 4096;
        let body = request(&[graph.ids.side.unwrap()], true);
        http::serve_with_selection(
            repo,
            Request {
                content_type: Some("application/x-git-upload-pack-request"),
                git_protocol: None,
                method: "POST",
                path: "git-upload-pack",
                query: None,
                body: &body,
            },
            &mut storage,
            &mut output,
        )
        .unwrap();
        pack::validate(&output.as_bytes()[8..]).unwrap();
    });
}
impl Repository for Streaming<'_> {
    fn head(&self) -> ObjectId {
        self.source.head()
    }
    fn object_count(&self) -> usize {
        self.source.object_count()
    }
    fn object_info(&self, index: usize) -> Result<ObjectInfo> {
        self.source.object_info(index)
    }
    fn object_id(&self, index: usize) -> Option<ObjectId> {
        self.source.object_id(index)
    }
    fn read_object(&self, index: usize, offset: u64, storage: &mut [u8]) -> Result<usize> {
        if self.fail.get() {
            return Err(Error::Backend);
        }
        let length = self.source.read_object(index, offset, storage)?;
        self.bytes.set(self.bytes.get() + length);
        Ok(length)
    }
}

#[test]
fn backend_failures_emit_nothing_and_a_fresh_selection_can_retry() {
    let graph = Graph::new();
    let source = Streaming {
        source: &graph,
        fail: Cell::new(false),
        bytes: Cell::new(0),
    };
    let repository = RefRepository::new(&source, graph.state(), &graph.refs).unwrap();
    validated(&repository, |repo| {
        source.fail.set(true);
        let (result, bytes) = serve_selected(repo, &[graph.head()], 1, CAPACITY, true);
        assert!(matches!(result, Err(Error::Backend)));
        assert!(bytes.is_empty());
        source.fail.set(false);
        let (result, bytes) = serve_selected(repo, &[graph.ids.side.unwrap()], 1, CAPACITY, true);
        assert_eq!(result.unwrap().status, 200);
        pack::validate(&bytes[8..]).unwrap();
    });
}
