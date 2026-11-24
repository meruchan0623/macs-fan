//! Independently written fan-channel labels from locally verified model/index facts.
//! No vendor resource or implementation is loaded or bundled. See fan-catalog-evidence.md.
use crate::catalog::{self, Evidence};

const SOURCE: &str = "evidence/deep/resources/fanInfo.xml";
const DUAL_NOTE: &str = "资料描述同一控制通道的双风扇组件；不代表枚举到两个独立风扇";

struct Rule {
    scope: &'static str,
    // Slot position is the hardware fan index; absence never creates a channel.
    labels: &'static [(&'static str, u16)],
    note: Option<&'static str>,
}

// Source order matters: the first matching model block wins, including absent slots.
const RULES: &[Rule] = &[
    Rule { scope: "iMacPro1,1", labels: &[("双风扇散热", 4)], note: Some(DUAL_NOTE) },
    Rule { scope: "MacBookPro15,1;MacBookPro15,2;MacBookPro15,3;MacBookPro16,1;MacBookPro16,2;MacBookPro16,4;MacBookPro18,1+", labels: &[("左侧", 7), ("右侧", 8)], note: None },
    Rule { scope: "MacBookPro15,4;MacBookPro16,3;MacBookPro17,1;MacBookPro17,2", labels: &[("右侧", 11)], note: None },
    Rule { scope: "MacBookPro20,1", labels: &[("左侧", 14)], note: None },
    Rule { scope: "MacBookAir8,1;MacBookAir8,2;MacBookAir9,1;ADP3,2;Macmini8,1+", labels: &[("排风", 17)], note: None },
    Rule { scope: "MacPro7,1", labels: &[("排风", 20), ("进风 1", 21), ("进风 2", 22), ("进风 3", 23)], note: None },
    Rule { scope: "iMac20,1;iMac20,2", labels: &[("主风扇", 26)], note: None },
    Rule { scope: "iMac21,1+", labels: &[("左侧", 29), ("右侧", 30)], note: None },
    Rule { scope: "MacStudio1,1+", labels: &[("双风扇排风", 33)], note: Some(DUAL_NOTE) },
];

pub struct Descriptor {
    pub name: String,
    pub evidence: Vec<Evidence>,
    pub note: Option<&'static str>,
}

/// Call only for an already enumerated hardware channel; this never determines fan count.
pub fn describe(model: &str, index: u32) -> Descriptor {
    let rule = RULES
        .iter()
        .find(|rule| catalog::matches_scope(rule.scope, catalog::normalize_model(model)));
    let Some((rule, (name, line))) =
        rule.and_then(|rule| rule.labels.get(index as usize).map(|label| (rule, label)))
    else {
        return Descriptor {
            name: format!("风扇 {}", u64::from(index) + 1),
            evidence: Vec::new(),
            note: None,
        };
    };
    let mut evidence = vec![Evidence {
        file: SOURCE,
        line: *line,
        model_scope: rule.scope,
    }];
    evidence.extend(catalog::normalization_evidence(model));
    Descriptor {
        name: (*name).into(),
        evidence,
        note: rule.note,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_matching_block_shadows_later_exact_rule() {
        let left = describe("MacBookPro20,1", 0);
        assert_eq!(left.name, "左侧");
        assert_eq!(left.evidence[0].line, 7); // later exact block at line 14 is shadowed
        assert_eq!(describe("MacBookPro20,1", 1).name, "右侧");
        assert_eq!(describe("MacBookPro17,1", 0).name, "右侧");
        assert_eq!(describe("MacBookPro17,1", 1).name, "风扇 2");
    }

    #[test]
    fn aliases_use_shared_normalization_and_retain_provenance() {
        for (index, name) in [(0, "左侧"), (1, "右侧")] {
            let mapped = describe("Mac16,8", index);
            assert_eq!(mapped.name, name);
            assert_eq!(mapped.evidence[1].line, 36);
            assert_eq!(mapped.evidence[1].model_scope, "Mac16,8");
        }
    }

    #[test]
    fn only_declared_indices_receive_labels() {
        assert_eq!(describe("MacBookAir8,1", 0).name, "排风");
        for model in ["MacBookAir8,1", "iMacPro1,1", "MacStudio1,1"] {
            assert_eq!(describe(model, 1).name, "风扇 2");
            assert!(describe(model, 1).evidence.is_empty());
        }
        assert!(describe("MacStudio1,1", 0).note.is_some());
        assert_eq!(describe("MacPro7,1", 3).name, "进风 3");
        assert_eq!(describe("MacPro7,1", 4).name, "风扇 5");
        assert_eq!(describe("unknown", u32::MAX).name, "风扇 4294967296");
    }

    #[test]
    fn shared_range_boundaries_and_malformed_models_are_respected() {
        assert_eq!(describe("MacBookPro18,0", 0).name, "风扇 1");
        assert_eq!(describe("MacBookPro18,1", 0).name, "左侧");
        assert_eq!(describe("iMac21,0", 0).name, "风扇 1");
        assert_eq!(describe("iMac21,1", 0).name, "左侧");
        for model in [
            "MacBookAir13,1",
            "Mac99,99",
            "MacBookPro018,1",
            "MacBookPro18,01",
            "MacBookPro18,1+",
            "MacBookPro18,1 ",
            " Mac16,8",
            "MacBookPro999999,1",
        ] {
            let descriptor = describe(model, 0);
            assert_eq!(descriptor.name, "风扇 1", "{model}");
            assert!(descriptor.evidence.is_empty());
            assert_eq!(descriptor.note, None);
        }
    }
}
