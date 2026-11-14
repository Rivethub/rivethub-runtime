use super::*;
use async_trait::async_trait;
use parking_lot::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

fn prepared(name: &str, type_name: &str) -> PreparedEntity {
    PreparedEntity {
        entity_ref: None,
        name: name.to_owned(),
        type_name: type_name.to_owned(),
        aliases: Vec::new(),
        facts: Vec::new(),
        history: Vec::new(),
    }
}

fn candidate(id: u128, name: &str, type_name: &str) -> MatchCandidate {
    MatchCandidate {
        id: Uuid::from_u128(id),
        name: name.to_owned(),
        type_name: type_name.to_owned(),
        aliases: Vec::new(),
        summary: String::new(),
        trigram: 0.5,
        embedding: 0.0,
    }
}

#[test]
fn a_low_confidence_choice_holds_and_a_sure_new_is_created() {
    let ranked = vec![candidate(4, "gpubox", "Hosts")];
    let held = apply_asked(
        &prepared("gpu server", "Hosts"),
        &ranked,
        Asked::Answer(RawDecision {
            choice: "0".to_owned(),
            confidence: 0.4,
        }),
        0.72,
    )
    .expect("a low confidence answer is a hold");
    assert!(matches!(held, MatchStep::Held { reason, .. } if reason == "low-confidence"));
    let created =
        apply_asked(&prepared("phil", "People"), &[], Asked::Skip, 0.72).expect("no candidates");
    assert!(matches!(created, MatchStep::Ready(item) if item.entity_ref.is_none()));
}

#[test]
fn an_exact_alias_binds_and_a_person_stays_a_second_page() {
    let id = Uuid::from_u128(9);
    let mut host = candidate(9, "gpubox", "Hosts");
    host.aliases = vec!["tvrt".to_owned()];
    let bound = apply_asked(
        &prepared("tvrt", "Hosts"),
        std::slice::from_ref(&host),
        Asked::Skip,
        0.72,
    )
    .expect("exact alias");
    assert!(matches!(bound, MatchStep::Ready(item) if item.entity_ref == Some(id)));
    let crossed = apply_asked(
        &prepared("rivet", "Hosts"),
        &[candidate(2, "rivet", "Projects")],
        Asked::Answer(RawDecision {
            choice: "new".to_owned(),
            confidence: 0.95,
        }),
        0.72,
    )
    .expect("exact name");
    assert!(
        matches!(crossed, MatchStep::Ready(item) if item.entity_ref == Some(Uuid::from_u128(2)))
    );
    let person = candidate(3, "phil", "People");
    let shown = offered(&prepared("phil", "Projects"), std::slice::from_ref(&person));
    assert_eq!(shown.len(), 0);
    let created = apply_asked(&prepared("phil", "Projects"), &shown, Asked::Skip, 0.72)
        .expect("person stays apart");
    assert!(matches!(created, MatchStep::Ready(item) if item.entity_ref.is_none()));
    let failed = apply_asked(
        &prepared("box", "Hosts"),
        &[candidate(5, "gpubox", "Hosts")],
        Asked::Unreachable(WorkerError::Llm {
            message: "down".to_owned(),
        }),
        0.72,
    );
    let Err(error) = failed else {
        panic!("an unreachable decider created the entity");
    };
    assert!(
        undecided(&error)
            .to_string()
            .contains("entity match undecided")
    );
}

struct Scripted {
    calls: AtomicUsize,
    choice: &'static str,
}

#[async_trait]
impl EntityDecider for Scripted {
    async fn decide(&self, request: &DecideRequest) -> Result<RawDecision, WorkerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if request.observation.starts_with("name:") {
            assert!(
                request.options.iter().all(|line| line.contains('(')),
                "{}",
                request.observation
            );
        }
        Ok(RawDecision {
            choice: self.choice.to_owned(),
            confidence: 0.93,
        })
    }

    async fn alias_probability(&self, _question: &str) -> Result<f32, WorkerError> {
        Ok(0.0)
    }
}

fn scripted(choice: &'static str) -> Scripted {
    Scripted {
        calls: AtomicUsize::new(0),
        choice,
    }
}

async fn judged(
    choice: &'static str,
    prepared: &PreparedEntity,
    ranked: &[MatchCandidate],
) -> (usize, MatchStep) {
    let shown = offered(prepared, ranked);
    let decider = scripted(choice);
    let asked = decision_for(&decider, prepared, &shown).await;
    let step = apply_asked(prepared, &shown, asked, 0.72).expect("the stub answered");
    (decider.calls.load(Ordering::SeqCst), step)
}

#[tokio::test]
async fn a_stub_decider_binds_each_compatible_pair_and_skips_the_rest() {
    let tools = [
        "Software",
        "Services",
        "Apps",
        "CLIs and tools",
        "Libraries and frameworks",
        "Plugins and skills",
    ];
    for (index, left) in tools.iter().enumerate() {
        for right in tools.iter().skip(index + 1) {
            for (observed_type, kept_type) in [(*left, *right), (*right, *left)] {
                let prepared = prepared("ComfyUI", observed_type);
                let ranked = [candidate(7, "comfyui", kept_type)];
                let (calls, step) = judged("0", &prepared, &ranked).await;
                let bound = matches!(
                    step,
                    MatchStep::Ready(item) if item.entity_ref == Some(Uuid::from_u128(7))
                );
                assert!(bound, "{observed_type} -> {kept_type} did not bind");
                assert_eq!(calls, 0, "{observed_type} -> {kept_type} asked the decider");
            }
        }
    }

    let host = candidate(1, "datahub", "Hosts");
    let service = prepared("Data Hub", "Services");
    let (calls, bound) = judged("new", &service, std::slice::from_ref(&host)).await;
    assert_eq!(calls, 0);
    assert!(matches!(bound, MatchStep::Ready(item) if item.entity_ref == Some(host.id)));

    let repo = candidate(3, "acme/datahub", "Repos");
    let named = prepared("datahub", "Services");
    let (calls, kept) = judged("new", &named, std::slice::from_ref(&repo)).await;
    assert_eq!(calls, 0);
    assert!(matches!(kept, MatchStep::Ready(item) if item.entity_ref == Some(repo.id)));
    let long = candidate(4, "bottlecapai/ThinkingCap-Qwen3.8-Flash-Next", "Repos");
    let model = prepared("ThinkingCap", "Models");
    assert_eq!(offered(&model, std::slice::from_ref(&long)).len(), 1);

    let same_type = candidate(8, "gpubox", "Hosts");
    let fuzzy = prepared("gpu server", "Hosts");
    assert_eq!(offered(&fuzzy, std::slice::from_ref(&same_type)).len(), 1);
    checked_name_pairs().await;
}

async fn checked_name_pairs() {
    let bound_pairs = [
        ("Models", "Services", "deepseek", "deepseek"),
        (
            "CLIs and tools",
            "Organizations",
            "huggingface",
            "huggingface",
        ),
        ("Agents", "Hosts", "grokbot", "grokbot"),
        ("Models", "Repos", "llama", "llama"),
        ("Projects", "Software", "RivetHub", "rivethub"),
    ];
    for (observed_type, kept_type, observed_name, kept_name) in bound_pairs {
        let prepared = prepared(observed_name, observed_type);
        let ranked = [candidate(9, kept_name, kept_type)];
        let (calls, step) = judged("new", &prepared, &ranked).await;
        assert_eq!(calls, 0, "{observed_type} vs {kept_type} asked the decider");
        assert!(
            matches!(step, MatchStep::Ready(item) if item.entity_ref == Some(Uuid::from_u128(9))),
            "{observed_type} vs {kept_type} did not bind"
        );
    }
    let person = prepared("phil", "Projects");
    let people = [candidate(9, "phil", "People")];
    let (calls, step) = judged("0", &person, &people).await;
    assert_eq!(calls, 0, "a person and a project were offered");
    assert!(matches!(step, MatchStep::Ready(item) if item.entity_ref.is_none()));
    let crossed = [
        ("Services", "Hosts", "datahub", "datahub-extra"),
        ("Services", "Repos", "web", "acme/web"),
    ];
    for (observed_type, kept_type, observed_name, kept_name) in crossed {
        let prepared = prepared(observed_name, observed_type);
        let ranked = [candidate(9, kept_name, kept_type)];
        let (calls, same) = judged("0", &prepared, &ranked).await;
        assert_eq!(calls, 1, "{observed_name} was not checked");
        assert!(
            matches!(same, MatchStep::Ready(item) if item.entity_ref == Some(Uuid::from_u128(9))),
            "{observed_name} was not bound"
        );
        let (calls, different) = judged("new", &prepared, &ranked).await;
        assert_eq!(calls, 1, "{observed_name} was checked twice");
        assert!(
            matches!(different, MatchStep::Ready(item) if item.entity_ref.is_none()),
            "{observed_name} was created as the same entity"
        );
    }

    let mut alias_only = candidate(5, "widget", "Hosts");
    alias_only.aliases = vec!["datahub".to_owned()];
    let via_alias = prepared("datahub", "Services");
    let (calls, step) = judged("new", &via_alias, std::slice::from_ref(&alias_only)).await;
    assert_eq!(calls, 0);
    assert!(matches!(step, MatchStep::Ready(item) if item.entity_ref == Some(alias_only.id)));
}

struct Flaky {
    calls: AtomicUsize,
}

#[async_trait]
impl EntityDecider for Flaky {
    async fn decide(&self, _request: &DecideRequest) -> Result<RawDecision, WorkerError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(WorkerError::Llm {
                message: "down".to_owned(),
            })
        } else {
            Ok(RawDecision {
                choice: "new".to_owned(),
                confidence: 0.95,
            })
        }
    }

    async fn alias_probability(&self, _question: &str) -> Result<f32, WorkerError> {
        Ok(0.0)
    }
}

#[test]
fn new_entity_keeps_aliases_the_mention_states() {
    let known = vec![known_entity(Uuid::new_v4(), "gpubox", "host", &[])];
    let aliases = vec![
        "G9".to_owned(),
        "gpubox".to_owned(),
        "11.22.33.45".to_owned(),
        "box_a".to_owned(),
    ];
    let mention = "gerty (G9, HP DL380 Gen9) at 11.22.33.45 is up";
    let kept = new_entity_aliases("gerty", &aliases, mention, &known);
    assert_eq!(kept, vec!["G9".to_owned(), "11.22.33.45".to_owned()]);
}

fn known_entity(id: Uuid, name: &str, type_name: &str, aliases: &[&str]) -> KnownEntity {
    KnownEntity {
        id,
        name: name.to_owned(),
        type_name: type_name.to_owned(),
        aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
        facts: vec![kernel::wiki::entity_pass::KnownFact {
            attribute: "summary".to_owned(),
            value: "gpu server".to_owned(),
        }],
    }
}

#[test]
fn gerty_with_gpubox_id_is_not_bound_directly() {
    let id = Uuid::from_u128(7);
    let known = vec![known_entity(id, "gpubox", "Hosts", &[])];
    let mut observed = prepared("gerty", "Hosts");
    observed.entity_ref = Some(id);
    observed.aliases = vec!["g9".to_owned(), "srv9".to_owned(), "11.22.33.45".to_owned()];
    let RefGate::Match { suggested } = ref_gate(&observed, &known) else {
        panic!("gerty was bound directly to gpubox");
    };
    let suggested = suggested.expect("gpubox stays in the candidate set");
    assert_eq!(suggested.id, id);
    assert_eq!(suggested.name, "gpubox");
    let mut ranked = Vec::new();
    inject_suggested(&mut ranked, Some(suggested));
    let mut cleared = observed;
    cleared.entity_ref = None;
    let shown = offered(&cleared, &ranked);
    assert_eq!(shown.len(), 1);
    assert_eq!(shown.first().map(|item| item.id), Some(id));
    assert!(cleared.entity_ref.is_none());
}

#[test]
fn an_existing_alias_is_bound_directly() {
    let id = Uuid::from_u128(7);
    let known = vec![known_entity(id, "gpubox", "Hosts", &["g9"])];
    let mut observed = prepared("G9", "Hosts");
    observed.entity_ref = Some(id);
    let gate = ref_gate(&observed, &known);
    assert!(
        matches!(gate, RefGate::Direct(subject) if subject.id == id && subject.name == "gpubox")
    );
}

struct AliasStub {
    score: f32,
    calls: AtomicUsize,
    question: Mutex<String>,
}

#[async_trait]
impl EntityDecider for AliasStub {
    async fn decide(&self, _request: &DecideRequest) -> Result<RawDecision, WorkerError> {
        Ok(RawDecision {
            choice: "new".to_owned(),
            confidence: 0.95,
        })
    }

    async fn alias_probability(&self, question: &str) -> Result<f32, WorkerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.question.lock() = question.to_owned();
        Ok(self.score)
    }
}

fn host_subject(aliases: &[&str]) -> AliasSubject {
    AliasSubject {
        id: Uuid::from_u128(7),
        name: "gpubox".to_owned(),
        type_name: "Hosts".to_owned(),
        summary: "gpu server".to_owned(),
        aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
    }
}

#[tokio::test]
async fn a_new_alias_is_attached_only_when_the_jev_check_passes() {
    let subject = host_subject(&[]);
    let known = vec![known_entity(subject.id, "gpubox", "Hosts", &[])];
    let positive = AliasStub {
        score: 0.91,
        calls: AtomicUsize::new(0),
        question: Mutex::new(String::new()),
    };
    let kept = filter_new_aliases(
        &positive,
        vec!["tvrt".to_owned()],
        &subject,
        "gpubox is also called tvrt",
        &known,
    )
    .await;
    assert_eq!(kept.kept, vec!["tvrt".to_owned()]);
    assert_eq!(positive.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        positive.question.lock().as_str(),
        "Transcript: \"gpubox is also called tvrt\". Entity: gpubox (Hosts; also known as none; gpu server). Does \"tvrt\" in this transcript refer to this same entity?"
    );
    let negative = AliasStub {
        score: 0.59,
        calls: AtomicUsize::new(0),
        question: Mutex::new(String::new()),
    };
    let dropped = filter_new_aliases(
        &negative,
        vec!["tvrt".to_owned()],
        &subject,
        "gpubox is also called tvrt",
        &known,
    )
    .await;
    assert_eq!(dropped.kept, Vec::<String>::new());
    assert_eq!(negative.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn a_tight_mention_pairs_the_name_and_the_alias() {
    let mention = "datahub (box42, 11.22.33.46)";
    assert!(alias_paired("datahub", "box42", mention));
    assert!(alias_paired("DataHub", "BOX42", mention));
    assert!(!alias_paired("datahub", "110", mention));
    assert!(alias_paired("box_a", "GERTY", "GERTY (box_a)"));
    assert!(alias_paired("box_a", "gerty", "box_a/GERTY"));
    assert!(alias_paired("box_a", "gerty", "box_a aka GERTY"));
    assert!(alias_paired("box_a", "gerty", "box_a = GERTY"));
    assert!(alias_paired("box_a", "gerty", "GERTY is box_a"));
    assert!(!alias_paired(
        "gpubox",
        "tvrt",
        "gpubox is also called tvrt"
    ));
    assert!(!alias_paired("datahub", "box42", "datahub is box42"));
    let near = format!("datahub{}(box42)", " ".repeat(29));
    let far = format!("datahub{}(box42)", " ".repeat(30));
    assert!(alias_paired("datahub", "box42", &near));
    assert!(!alias_paired("datahub", "box42", &far));
}

#[tokio::test]
async fn box42_paired_with_datahub_is_accepted_without_jev() {
    let subject = AliasSubject {
        id: Uuid::from_u128(11),
        name: "datahub".to_owned(),
        type_name: "Hosts".to_owned(),
        summary: "proxmox container".to_owned(),
        aliases: Vec::new(),
    };
    let stub = AliasStub {
        score: 0.0,
        calls: AtomicUsize::new(0),
        question: Mutex::new(String::new()),
    };
    let kept = filter_new_aliases(
        &stub,
        vec!["box42".to_owned()],
        &subject,
        "datahub (box42, 11.22.33.46)",
        &[],
    )
    .await;
    assert_eq!(kept.kept, vec!["box42".to_owned()]);
    assert_eq!(stub.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_contextual_alias_at_065_is_kept_and_030_is_dropped() {
    let subject = host_subject(&["g9", "srv9"]);
    let mention = "jobs moved onto the box";
    let positive = AliasStub {
        score: 0.65,
        calls: AtomicUsize::new(0),
        question: Mutex::new(String::new()),
    };
    let kept = filter_new_aliases(&positive, vec!["tvrt".to_owned()], &subject, mention, &[]).await;
    assert_eq!(kept.kept, vec!["tvrt".to_owned()]);
    assert_eq!(positive.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        positive.question.lock().as_str(),
        "Transcript: \"jobs moved onto the box\". Entity: gpubox (Hosts; also known as g9, srv9; gpu server). Does \"tvrt\" in this transcript refer to this same entity?"
    );
    let negative = AliasStub {
        score: 0.3,
        calls: AtomicUsize::new(0),
        question: Mutex::new(String::new()),
    };
    let dropped = filter_new_aliases(
        &negative,
        vec!["unrelated".to_owned()],
        &subject,
        mention,
        &[],
    )
    .await;
    assert_eq!(dropped.kept, Vec::<String>::new());
    assert_eq!(negative.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_alias_equal_to_another_entity_name_is_rejected() {
    let subject = host_subject(&[]);
    let known = vec![
        known_entity(subject.id, "gpubox", "Hosts", &[]),
        known_entity(Uuid::from_u128(8), "gerty", "Hosts", &[]),
    ];
    let stub = AliasStub {
        score: 0.99,
        calls: AtomicUsize::new(0),
        question: Mutex::new(String::new()),
    };
    let kept = filter_new_aliases(
        &stub,
        vec!["gerty".to_owned()],
        &subject,
        "gerty showed up in the logs",
        &known,
    )
    .await;
    assert_eq!(kept.kept, Vec::<String>::new());
    assert!(kept.merges.is_empty());
    assert_eq!(stub.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_explicit_pairing_merges_two_hosts_at_or_above_0_8() {
    let subject = AliasSubject {
        id: Uuid::from_u128(7),
        name: "GERTY".to_owned(),
        type_name: "Hosts".to_owned(),
        summary: "proxmox host".to_owned(),
        aliases: vec!["oldbox".to_owned()],
    };
    let known = vec![
        known_entity(subject.id, "GERTY", "Hosts", &["oldbox"]),
        known_entity(Uuid::from_u128(8), "box_a", "Hosts", &["G9"]),
    ];
    let mention = "GERTY (old box_a/G9, 11.22.33.45) serves Qwen 27B";
    let stub = AliasStub {
        score: 0.8,
        calls: AtomicUsize::new(0),
        question: Mutex::new(String::new()),
    };
    let merged = filter_new_aliases(&stub, vec!["G9".to_owned()], &subject, mention, &known).await;
    assert_eq!(merged.kept, Vec::<String>::new());
    assert_eq!(merged.merges.len(), 1);
    assert_eq!(
        merged.merges.first().map(|item| item.other_id),
        Some(Uuid::from_u128(8))
    );
    assert_eq!(
        merged.merges.first().map(|item| item.other_name.as_str()),
        Some("box_a")
    );
    let score = merged.merges.first().map(|item| item.score);
    assert!(score.is_some_and(|value| (value - 0.8).abs() < 0.001));
    assert_eq!(stub.calls.load(Ordering::SeqCst), 1);
    let question = stub.question.lock().clone();
    assert!(question.contains(mention));
    assert!(question.contains("GERTY"));
    assert!(question.contains("box_a"));
    assert!(question.contains("Hosts"));
    assert!(question.contains("oldbox"));
    assert!(question.contains("G9"));
    assert!(question.contains("proxmox host"));
    assert!(question.contains("gpu server"));
}

#[tokio::test]
async fn a_merge_below_0_8_or_across_types_does_not_fire() {
    let subject = host_subject(&[]);
    let hosts = vec![
        known_entity(subject.id, "gpubox", "Hosts", &[]),
        known_entity(Uuid::from_u128(8), "gerty", "Hosts", &[]),
    ];
    let low = AliasStub {
        score: 0.79,
        calls: AtomicUsize::new(0),
        question: Mutex::new(String::new()),
    };
    let dropped = filter_new_aliases(
        &low,
        vec!["gerty".to_owned()],
        &subject,
        "gpubox (gerty)",
        &hosts,
    )
    .await;
    assert_eq!(dropped.kept, Vec::<String>::new());
    assert!(dropped.merges.is_empty());
    assert_eq!(low.calls.load(Ordering::SeqCst), 1);
    let project = AliasStub {
        score: 0.99,
        calls: AtomicUsize::new(0),
        question: Mutex::new(String::new()),
    };
    let known = vec![
        known_entity(subject.id, "gpubox", "Hosts", &[]),
        known_entity(Uuid::from_u128(9), "acme", "Projects", &[]),
    ];
    let apart = filter_new_aliases(
        &project,
        vec!["acme".to_owned()],
        &subject,
        "gpubox (acme)",
        &known,
    )
    .await;
    assert!(apart.merges.is_empty());
    assert_eq!(project.calls.load(Ordering::SeqCst), 0);
    let person = AliasSubject {
        id: Uuid::from_u128(3),
        name: "Phil".to_owned(),
        type_name: "People".to_owned(),
        summary: "a person".to_owned(),
        aliases: Vec::new(),
    };
    let mixed = vec![
        known_entity(person.id, "Phil", "People", &[]),
        known_entity(Uuid::from_u128(4), "philbox", "Hosts", &[]),
    ];
    let blocked = filter_new_aliases(
        &project,
        vec!["philbox".to_owned()],
        &person,
        "Phil (philbox)",
        &mixed,
    )
    .await;
    assert!(blocked.merges.is_empty());
}

#[tokio::test]
async fn an_ip_alias_not_tied_in_the_mention_is_rejected() {
    let subject = host_subject(&[]);
    let stub = AliasStub {
        score: 0.99,
        calls: AtomicUsize::new(0),
        question: Mutex::new(String::new()),
    };
    let mention = "G9 is `gerty` at `11.22.33.45`";
    let kept = filter_new_aliases(
        &stub,
        vec!["11.22.33.45".to_owned()],
        &subject,
        mention,
        &[],
    )
    .await;
    assert_eq!(kept.kept, Vec::<String>::new());
    assert_eq!(stub.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_decide_error_is_retried_once() {
    let decision = ask(
        &Flaky {
            calls: AtomicUsize::new(0),
        },
        &DecideRequest {
            observation: "name: phil".to_owned(),
            options: vec!["Phil (People)".to_owned()],
        },
    )
    .await
    .unwrap();
    assert_eq!(decision.choice, "new");
    assert_eq!(decision.confidence, 0.95);
}

#[tokio::test]
async fn a_near_duplicate_is_created_only_when_the_decider_says_different() {
    let ranked = [candidate(4, "gerty", "Hosts")];
    let prepared = prepared("box9", "Hosts");
    let (calls, same) = judged("0", &prepared, &ranked).await;
    assert_eq!(calls, 1);
    assert!(matches!(same, MatchStep::Ready(item) if item.entity_ref == Some(Uuid::from_u128(4))));
    let (calls, different) = judged("new", &prepared, &ranked).await;
    assert_eq!(calls, 1);
    assert!(matches!(different, MatchStep::Ready(item) if item.entity_ref.is_none()));
}

#[tokio::test]
async fn an_unreachable_decider_is_asked_twice_and_creates_nothing() {
    let ranked = [candidate(4, "gerty", "Hosts")];
    let prepared = prepared("box9", "Hosts");
    let down = Down {
        calls: AtomicUsize::new(0),
    };
    let asked = decision_for(&down, &prepared, &ranked).await;
    assert_eq!(down.calls.load(Ordering::SeqCst), 2);
    match apply_asked(&prepared, &ranked, asked, 0.72) {
        Err(error) => assert!(
            undecided(&error)
                .to_string()
                .contains("entity match undecided")
        ),
        Ok(_) => panic!("created"),
    }
}

#[tokio::test]
async fn a_service_and_a_host_are_one_entity_when_the_decider_says_same() {
    let ranked = [candidate(8, "gerty", "Hosts")];
    let prepared = prepared("box9", "Services");
    let (calls, same) = judged("0", &prepared, &ranked).await;
    assert_eq!(calls, 1);
    assert!(matches!(same, MatchStep::Ready(item) if item.entity_ref == Some(Uuid::from_u128(8))));
    let (calls, different) = judged("new", &prepared, &ranked).await;
    assert_eq!(calls, 1);
    assert!(matches!(different, MatchStep::Ready(item) if item.entity_ref.is_none()));
}

#[test]
fn a_decision_records_the_name_the_candidates_and_the_model() {
    let ranked = [candidate(4, "gerty", "Hosts")];
    let prepared = prepared("box9", "Services");
    let decision = entity_decision(
        &prepared,
        &ranked,
        &Ruling {
            verdict: "same",
            choice: "0",
            confidence: 0.93,
            model: "thinkingcap-flash-next",
            entity_id: Some(Uuid::from_u128(4)),
        },
    );
    assert_eq!(
        decision,
        MatchDecision {
            kind: "entity".to_owned(),
            proposed_name: "box9".to_owned(),
            proposed_type: "Services".to_owned(),
            candidates: vec![format!("gerty (Hosts) {}", Uuid::from_u128(4))],
            verdict: "same".to_owned(),
            choice: "0".to_owned(),
            confidence: 0.93,
            model: "thinkingcap-flash-next".to_owned(),
            entity_id: Some(Uuid::from_u128(4)),
        }
    );
}

struct TypeStub {
    calls: AtomicUsize,
    choice: &'static str,
    confidence: f32,
}

#[async_trait]
impl EntityDecider for TypeStub {
    async fn decide(&self, request: &DecideRequest) -> Result<RawDecision, WorkerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(
            request.observation.starts_with("proposed type:"),
            "{}",
            request.observation
        );
        Ok(RawDecision {
            choice: self.choice.to_owned(),
            confidence: self.confidence,
        })
    }

    async fn alias_probability(&self, _question: &str) -> Result<f32, WorkerError> {
        Ok(0.0)
    }
}

fn type_stub(choice: &'static str) -> TypeStub {
    TypeStub {
        calls: AtomicUsize::new(0),
        choice,
        confidence: 0.93,
    }
}

fn brief(name: &str, aliases: &[&str], examples: &[&str]) -> TypeBrief {
    TypeBrief {
        name: name.to_owned(),
        slug: name.to_ascii_lowercase(),
        aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
        examples: examples
            .iter()
            .map(|example| (*example).to_owned())
            .collect(),
    }
}

#[tokio::test]
async fn a_new_type_follows_the_decider_once() {
    let existing = vec![brief("Hosts", &[], &["gerty"]), brief("Services", &[], &[])];
    let same = type_stub("0");
    let settled = settle_type(&same, "machine", &existing).await;
    assert_eq!(same.calls.load(Ordering::SeqCst), 1);
    let TypeOutcome::Use { name, decision } = settled else {
        panic!("machine was not filed under the existing type");
    };
    assert_eq!(name, "Hosts");
    assert_eq!(decision.verdict, "same");
    assert_eq!(decision.model, "decider");
    assert_eq!(decision.kind, "type");
    assert!(
        decision
            .candidates
            .iter()
            .any(|line| line.contains("examples: gerty"))
    );
    assert_eq!(alias_to_keep("machine", &name).as_deref(), Some("machine"));

    let different = type_stub("new");
    let settled = settle_type(&different, "machine", &existing).await;
    assert_eq!(different.calls.load(Ordering::SeqCst), 1);
    let TypeOutcome::Use { name, decision } = settled else {
        panic!("a different type was not kept");
    };
    assert_eq!(name, "machine");
    assert_eq!(decision.verdict, "different");
    assert_eq!(alias_to_keep("machine", &name), None);

    let folded = type_stub("0");
    let settled = settle_type(&folded, "host", &existing).await;
    assert_eq!(folded.calls.load(Ordering::SeqCst), 0);
    let TypeOutcome::Use { name, decision } = settled else {
        panic!("host was asked");
    };
    assert_eq!(name, "Hosts");
    assert_eq!(decision.model, "exact");
    assert_eq!(alias_to_keep("host", &name).as_deref(), Some("host"));
    assert_eq!(alias_to_keep("Hosts", &name), None);

    let people = vec![brief("People", &[], &[])];
    let person = type_stub("new");
    let settled = settle_type(&person, "person", &people).await;
    assert_eq!(person.calls.load(Ordering::SeqCst), 1);
    let TypeOutcome::Use { name, decision } = settled else {
        panic!("person was folded into People");
    };
    assert_eq!(name, "person");
    assert_eq!(decision.verdict, "different");

    let aliased = vec![brief("Hosts", &["machine"], &["gerty"])];
    let again = type_stub("0");
    let settled = settle_type(&again, "machine", &aliased).await;
    assert_eq!(again.calls.load(Ordering::SeqCst), 0);
    let TypeOutcome::Use { name, decision } = settled else {
        panic!("a stored alias was asked");
    };
    assert_eq!(name, "Hosts");
    assert_eq!(decision.verdict, "same");
    assert_eq!(decision.model, "exact");

    let empty = type_stub("0");
    let settled = settle_type(&empty, "machine", &[]).await;
    assert_eq!(empty.calls.load(Ordering::SeqCst), 0);
    let TypeOutcome::Use { name, decision } = settled else {
        panic!("an empty catalogue asked");
    };
    assert_eq!(name, "machine");
    assert_eq!(decision.verdict, "none");
    assert_eq!(decision.model, "none");

    let unsure = TypeStub {
        calls: AtomicUsize::new(0),
        choice: "0",
        confidence: 0.4,
    };
    let settled = settle_type(&unsure, "machine", &existing).await;
    assert_eq!(unsure.calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        settled,
        TypeOutcome::Unsure { decision } if decision.verdict == "hold"
    ));
}

struct Down {
    calls: AtomicUsize,
}

#[async_trait]
impl EntityDecider for Down {
    async fn decide(&self, _request: &DecideRequest) -> Result<RawDecision, WorkerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(WorkerError::Llm {
            message: "connection refused".to_owned(),
        })
    }

    async fn alias_probability(&self, _question: &str) -> Result<f32, WorkerError> {
        Ok(0.0)
    }
}

#[tokio::test]
async fn an_unreachable_type_decider_is_asked_twice_and_creates_nothing() {
    let existing = vec![brief("Hosts", &[], &[])];
    let down = Down {
        calls: AtomicUsize::new(0),
    };
    let settled = settle_type(&down, "machine", &existing).await;
    assert_eq!(down.calls.load(Ordering::SeqCst), 2);
    match settled {
        TypeOutcome::Unreachable { decision, error } => {
            assert_eq!(decision.verdict, "unreachable");
            assert_eq!(decision.model, "decider");
            assert!(decision.candidates.iter().any(|line| line == "Hosts"));
            assert!(error.to_string().contains("type match undecided"));
        }
        TypeOutcome::Use { .. } | TypeOutcome::Unsure { .. } => {
            panic!("an unreachable decider created the type");
        }
    }
}
