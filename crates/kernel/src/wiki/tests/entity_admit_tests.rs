use super::*;
use crate::wiki::entity_pass::KnownFact;
use crate::wiki::entity_pass::ObservedFact;
use uuid::Uuid;

fn kept(name: &str) -> bool {
    matches!(admit_name(name), Admit::Keep)
}

fn dropped(name: &str, kind: DropKind) -> bool {
    admit_name(name) == Admit::Drop(kind)
}

#[test]
fn an_owner_slash_repo_name_is_an_entity_and_a_real_path_is_not() {
    assert!(kept("philbert440/rivethub-runtime"));
    assert!(kept("1CatAI/1Cat-vLLM"));
    assert!(kept("bottlecapai/ThinkingCap-Qwen3.8-Flash-Next-NVFP4"));
    assert!(dropped("/nvme/tcfn/tc-nvfp4", DropKind::Path));
    assert!(dropped("kit/capture_tc.py", DropKind::Path));
    assert!(dropped("opt/work/tcfn/logs", DropKind::Path));
}

fn observation(name: &str, type_name: &str, fact: &str) -> EntityObservation {
    EntityObservation {
        name: name.to_owned(),
        type_name: type_name.to_owned(),
        facts: vec![ObservedFact {
            attribute: "note".to_owned(),
            value: fact.to_owned(),
            ..ObservedFact::default()
        }],
        ..EntityObservation::default()
    }
}

#[test]
fn denylists_are_sorted_for_lookup() {
    assert!(FILE_SUFFIXES.is_sorted());
    assert!(TOOLS.is_sorted());
    assert!(CLIS.is_sorted());
    assert!(GENERICS.is_sorted());
}

#[test]
fn mechanical_admission_rejects_paths_addresses_indexes_tools_and_commands() {
    assert!(dropped("/opt/work/tcfn/logs/ab2.log", DropKind::Path));
    assert!(dropped("capture-tc.py", DropKind::Path));
    assert!(dropped("capture-tc-py", DropKind::Path));
    assert!(dropped("rivet-shared-plans-cutover.md", DropKind::Path));
    assert!(dropped("https://example.com/a", DropKind::Path));
    assert!(dropped("11.22.33.44", DropKind::Address));
    assert!(dropped("11.22.33.0/24", DropKind::Address));
    assert!(dropped("10-4-20-207", DropKind::Address));
    assert!(dropped("gpu-0", DropKind::Index));
    assert!(dropped("0", DropKind::Index));
    assert!(dropped("eth0", DropKind::Index));
    assert!(dropped("toolsearch", DropKind::Tool));
    assert!(dropped("pushnotification", DropKind::Tool));
    assert!(dropped("taskstop", DropKind::Tool));
    assert!(dropped("read", DropKind::Tool));
    assert!(dropped("grep", DropKind::Cli));
    assert!(dropped("nvidia-smi", DropKind::Cli));
    assert!(dropped("pkill", DropKind::Cli));
    assert!(dropped("config", DropKind::Generic));
    assert!(dropped("kernel", DropKind::Generic));
    assert!(dropped("root", DropKind::Generic));
    assert!(kept("gpubox"));
    assert!(kept("jj"));
    assert!(kept("g9"));
    assert!(kept("box42"));
    assert!(kept("deepseek-v4.1"));
    assert!(kept("thinkingcap"));
}

#[test]
fn an_ip_or_chassis_of_a_host_is_an_alias_not_a_page() {
    let host = KnownEntity {
        id: Uuid::from_u128(7),
        name: "gpubox".to_owned(),
        type_name: "Hosts".to_owned(),
        aliases: Vec::new(),
        facts: vec![KnownFact {
            attribute: "address".to_owned(),
            value: "reachable at 11.22.33.44".to_owned(),
        }],
    };
    let screened = screen_observations(
        vec![observation("11.22.33.44", "Hosts", "ssh target")],
        &[host],
    );
    let page = screened.first().expect("folded onto the host");
    assert_eq!(page.entity_ref, Some(Uuid::from_u128(7)));
    assert_eq!(page.name, "gpubox");
    assert!(page.aliases.iter().any(|alias| alias == "11.22.33.44"));
    let chassis = identifying_aliases("Hosts", "Supermicro SYS-4029GP-TVRT at 11.22.33.44");
    assert!(chassis.iter().any(|alias| alias == "11.22.33.44"));
    assert!(
        chassis
            .iter()
            .any(|alias| alias.eq_ignore_ascii_case("SYS-4029GP-TVRT"))
    );
    assert!(
        chassis
            .iter()
            .any(|alias| alias.eq_ignore_ascii_case("TVRT"))
    );
}

#[test]
fn a_chassis_observation_folds_into_the_host_and_types_stay_apart() {
    let screened = screen_observations(
        vec![
            observation("SYS-4029GP-TVRT", "Hardware", "the chassis"),
            observation("gpubox", "Hosts", "Supermicro SYS-4029GP-TVRT"),
        ],
        &[],
    );
    assert_eq!(screened.len(), 1);
    let page = screened.first().expect("one host");
    assert_eq!(page.name, "gpubox");
    assert_eq!(page.type_name, "Hosts");
    let apart = screen_observations(
        vec![
            observation("rivet", "Hosts", "a machine"),
            observation("rivet", "Projects", "a project"),
        ],
        &[],
    );
    assert_eq!(apart.len(), 2);
}

#[test]
fn an_alias_absent_from_the_evidence_or_another_entity_name_is_dropped() {
    let evidence = "Phil restarted the desk. NFS is mounted.";
    let kept = evidenced_aliases(
        "Phil",
        &["aliasdesk".to_owned(), "Phil".to_owned(), "desk".to_owned()],
        evidence,
    );
    assert_eq!(kept, vec!["desk".to_owned()]);
    assert!(!alias_in_evidence("rivetos", evidence));
    assert!(alias_in_evidence("NFS", evidence));
    let nvidia = KnownEntity {
        id: Uuid::from_u128(4),
        name: "nvidia".to_owned(),
        type_name: "Organizations".to_owned(),
        aliases: Vec::new(),
        facts: Vec::new(),
    };
    assert!(alias_claims_known("NVIDIA", "V100", &[nvidia]));
    assert!(!alias_claims_known("V100", "V100", &[]));
    let mention = "G9 is `gerty` at `11.22.33.45`";
    assert!(address_tied_to_name("11.22.33.45", "gerty", mention));
    assert!(address_tied_to_name("11.22.33.45", "G9", mention));
    assert!(!address_tied_to_name("11.22.33.45", "gpubox", mention));
    assert!(!address_tied_to_name("g9", "gerty", mention));
    assert!(alias_is_address("11.22.33.44"));
}

#[test]
fn a_paired_name_or_alias_of_a_compatible_entity_is_a_merge_partner() {
    let gerty = Uuid::from_u128(1);
    let box_a = Uuid::from_u128(2);
    let person = Uuid::from_u128(3);
    let known = vec![
        known(gerty, "GERTY", "Hosts", &[]),
        known(box_a, "box_a", "Hosts", &["G9"]),
        known(person, "Phil", "People", &[]),
    ];
    let partner = merge_partner("G9", gerty, "Hosts", &known).map(|entity| entity.id);
    assert_eq!(partner, Some(box_a));
    assert_eq!(
        merge_partner("box_a", gerty, "Hosts", &known).map(|entity| entity.name.as_str()),
        Some("box_a")
    );
    assert!(merge_partner("G9", box_a, "Hosts", &known).is_none());
    assert!(merge_partner("Phil", gerty, "Hosts", &known).is_none());
    assert!(merge_partner("box_a", gerty, "Projects", &known).is_none());
    let mention = "GERTY (old box_a/G9, 11.22.33.45) serves Qwen 27B";
    assert!(names_share_mention("GERTY", "box_a", mention));
    assert!(names_share_mention("GERTY", "G9", mention));
    assert!(!names_share_mention("GERTY", "box_a", "box_a was rebooted"));
    assert!(!names_share_mention("g9", "host", "g90 host"));
}

fn known(id: Uuid, name: &str, type_name: &str, aliases: &[&str]) -> KnownEntity {
    KnownEntity {
        id,
        name: name.to_owned(),
        type_name: type_name.to_owned(),
        aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
        facts: Vec::new(),
    }
}
