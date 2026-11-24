//! Intel SMC description facts with local source-line provenance.
//! Candidate metadata never proves key presence, core count, or reading quality.
//! See ../../docs/intel-catalog-evidence.md. No vendor XML is embedded.

use crate::catalog::{
    normalize_model, resolve_rules_with_source, sensor, AggregateFact, Descriptor, Rule, SensorFact,
};

const SOURCE: &str = "evidence/deep/resources/smcSensors.xml";

// x86_64 isSameSensor: case-insensitive whole comparison AND exact first three
// characters. SMC keys are four ASCII bytes; never fold any prefix character.
pub(crate) fn keys_match(declared: &str, actual: &str) -> bool {
    declared.len() == 4
        && actual.len() == 4
        && declared.is_ascii()
        && actual.is_ascii()
        && declared.as_bytes()[..3] == actual.as_bytes()[..3]
        && declared.as_bytes()[3].eq_ignore_ascii_case(&actual.as_bytes()[3])
}

// All 27 model blocks, including the empty block and six exclusions, in source order.
const RULES: &[Rule] = &[
    Rule {
        scope: "all",
        sensors: &[
            sensor("TA0P", "环境1", "Other", 4, &[]),
            sensor("TA1P", "环境2", "Other", 5, &[]),
            sensor("TB0T", "电池最高温", "Battery", 6, &[]),
            sensor("TB1T", "电池传感器1", "Battery", 7, &[]),
            sensor("TB2T", "电池传感器2", "Battery", 8, &[]),
            sensor("TB3T", "电池", "Battery", 9, &[]),
            sensor("TC0c", "CPU核心1", "CPU", 11, &[]),
            sensor("TC1c", "CPU核心2", "CPU", 12, &[]),
            sensor("TC2c", "CPU核心3", "CPU", 13, &[]),
            sensor("TC3c", "CPU核心4", "CPU", 14, &[]),
            sensor("TC4c", "CPU核心5", "CPU", 15, &[]),
            sensor("TC5c", "CPU核心6", "CPU", 16, &[]),
            sensor("TC6c", "CPU核心7", "CPU", 17, &[]),
            sensor("TC7c", "CPU核心8", "CPU", 18, &[]),
            sensor("TC8c", "CPU核心9", "CPU", 19, &[]),
            sensor("TC9c", "CPU核心10", "CPU", 20, &[]),
            sensor("TCXC", "CPU PECI温度", "CPU", 22, &[]),
            sensor("TC0H", "CPU散热器", "CPU", 23, &[]),
            sensor("TC0D", "CPU二极管", "CPU", 24, &[]),
            sensor("TC0P", "CPU邻近", "CPU", 25, &[]),
            sensor("TCGC", "GPU PECI温度", "GPU", 26, &[]),
            sensor("TG0D", "GPU二极管1", "GPU", 27, &[]),
            sensor("TGDD", "GPU二极管2", "GPU", 28, &[]),
            sensor("TG0H", "GPU散热器", "GPU", 29, &[]),
            sensor("TG0P", "GPU邻近1", "GPU", 30, &[]),
            sensor("TG1P", "GPU邻近2", "GPU", 31, &[]),
            sensor("TO0P", "光驱", "Other", 32, &[]),
            sensor("TL0p", "显示屏邻近1", "Other", 33, &[]),
            sensor("TL1P", "显示屏邻近2", "Other", 34, &[]),
            sensor("Th1H", "散热器邻近", "Other", 35, &[]),
            sensor("TM0P", "内存邻近", "Other", 36, &[]),
            sensor("TM0S", "内存插槽", "Other", 37, &[]),
            sensor("TN0D", "MCP二极管", "Other", 38, &[]),
            sensor("TN0H", "MCP散热器", "Other", 39, &[]),
            sensor("TN0P", "北桥邻近", "Other", 40, &[]),
            sensor("Tp0P", "电源邻近", "Other", 41, &[]),
            sensor("Tp3H", "电源初级", "Other", 42, &[]),
            sensor("Tp0C", "电源AC/DC通道1", "Other", 43, &[]),
            sensor("TI0P", "雷雳接口邻近", "Other", 44, &[]),
            sensor("Ts0P", "触控板", "Other", 45, &[]),
            sensor("TW0P", "无线模块邻近", "Other", 46, &[]),
            sensor("TPCD", "平台控制器温度", "Other", 47, &[]),
        ],
        aggregates: &[
            AggregateFact { id: "avg:cpu", name: "CPU核心均值", group: "CPU", line: 10, hide_sensors: false, members: &["TC0c", "TC1c", "TC2c", "TC3c", "TC4c", "TC5c", "TC6c", "TC7c", "TC8c", "TC9c"] },
        ],
    },
    Rule {
        scope: "Macmini1,1",
        sensors: &[
            sensor("TC0P", "CPU稳压区", "CPU", 50, &[]),
            sensor("TC1P", "CPU邻近", "CPU", 51, &[]),
        ],
        aggregates: &[
        ],
    },
    Rule {
        scope: "Macmini3,1",
        sensors: &[
            sensor("TN0P", "MCP邻近", "Other", 54, &[]),
            sensor("TH0P", "硬盘外壳", "Other", 55, &[]),
        ],
        aggregates: &[
        ],
    },
    Rule {
        scope: "MacBookAir2,1",
        sensors: &[
            sensor("Tp1P", "主板前缘", "Other", 58, &[]),
            sensor("Ts0P", "外壳邻近", "Other", 59, &[]),
            sensor("TV0P", "排风口", "Other", 60, &[]),
        ],
        aggregates: &[
        ],
    },
    Rule {
        scope: "MacBook5,2",
        sensors: &[
            sensor("Th0H", "热管散热器", "Other", 63, &[]),
        ],
        aggregates: &[
        ],
    },
    Rule {
        scope: "MacBook8,1",
        sensors: &[
        ],
        aggregates: &[
        ],
    },
    Rule {
        scope: "MacBookPro2,1;MacBookPro2,2;MacBookPro3,1;MacBookPro4,1",
        sensors: &[
            sensor("Tm0P", "时钟芯片邻近", "Other", 67, &[]),
        ],
        aggregates: &[
        ],
    },
    Rule {
        scope: "MacBookPro5,1",
        sensors: &[
            sensor("TN0D", "CPU核心供电区", "CPU", 70, &[]),
        ],
        aggregates: &[
        ],
    },
    Rule {
        scope: "MacBookPro5,1;MacBookPro5,2;MacBookPro5,3",
        sensors: &[
            sensor("TN0P", "MCP邻近", "Other", 73, &[]),
            sensor("Tm0P", "电池充电区", "Other", 74, &[]),
            sensor("TG0H", "左侧散热器", "Other", 75, &[]),
            sensor("TG1H", "左侧散热器邻近", "Other", 76, &[]),
            sensor("Th2H", "右侧散热器", "Other", 77, &[]),
        ],
        aggregates: &[
        ],
    },
    Rule {
        scope: "MacBookPro6,1;MacBookPro6,2",
        sensors: &[
            sensor("Tm0P", "主板", "Other", 80, &[]),
        ],
        aggregates: &[
        ],
    },
    Rule {
        scope: "MacBookPro5,1;MacBookPro5,2;MacBookPro5,3;MacBookPro6,2",
        sensors: &[
            sensor("TG0H", "GPU模拟二极管", "Other", 83, &[]),
        ],
        aggregates: &[
        ],
    },
    Rule {
        scope: "Macmini8,1;MacBook8,1+;MacBookAir6,1+;MacBookPro9,1;MacBookPro9,2;MacBookPro10,1;MacBookPro11,2;MacBookPro11,3;MacBookPro11,4;MacBookPro11,5;MacBookPro13,2+",
        sensors: &[
            SensorFact { exclude: true, ..sensor("TC0c", "排除项", "Other", 86, &[]) },
            sensor("TC1C", "CPU核心1", "CPU", 88, &[]),
            sensor("TC2C", "CPU核心2", "CPU", 89, &[]),
            sensor("TC3C", "CPU核心3", "CPU", 90, &[]),
            sensor("TC4C", "CPU核心4", "CPU", 91, &[]),
            sensor("TC5C", "CPU核心5", "CPU", 92, &[]),
            sensor("TC6C", "CPU核心6", "CPU", 93, &[]),
            sensor("TC7C", "CPU核心7", "CPU", 94, &[]),
            sensor("TC8C", "CPU核心8", "CPU", 95, &[]),
        ],
        aggregates: &[
            AggregateFact { id: "avg:cpu", name: "CPU核心均值", group: "CPU", line: 87, hide_sensors: false, members: &["TC1C", "TC2C", "TC3C", "TC4C", "TC5C", "TC6C", "TC7C", "TC8C"] },
        ],
    },
    Rule {
        scope: "MacBookPro8,3",
        sensors: &[
            SensorFact { exclude: true, ..sensor("TC4c", "排除项", "Other", 99, &[]) },
        ],
        aggregates: &[
        ],
    },
    Rule {
        scope: "iMac13,2;iMac14,2;iMac15,1;iMac17,1;iMacPro1,1;MacPro1,1;MacPro2,1;MacPro3,1;MacPro4,1;MacPro5,1",
        sensors: &[
            sensor("TM0p", "内存模块1邻近", "Other", 102, &[]),
            sensor("TM1p", "内存模块2邻近", "Other", 103, &[]),
            sensor("TM2p", "内存模块3邻近", "Other", 104, &[]),
            sensor("TM3p", "内存模块4邻近", "Other", 105, &[]),
        ],
        aggregates: &[
        ],
    },
    Rule {
        scope: "iMac9,1;iMac10,1",
        sensors: &[
            sensor("TN0P", "MCP邻近", "Other", 108, &[]),
        ],
        aggregates: &[
        ],
    },
    Rule {
        scope: "MacPro1,1;MacPro2,1;MacPro3,1",
        sensors: &[
            sensor("TC0C", "CPU A核心0", "CPU", 111, &[]),
            sensor("TC1C", "CPU A核心1", "CPU", 112, &[]),
            sensor("TC2C", "CPU B核心0", "CPU", 113, &[]),
            sensor("TC3C", "CPU B核心1", "CPU", 114, &[]),
            sensor("TH0P", "硬盘位0", "Other", 115, &[]),
            sensor("TH1P", "硬盘位1", "Other", 116, &[]),
            sensor("TH2P", "硬盘位2", "Other", 117, &[]),
            sensor("TH3P", "硬盘位3", "Other", 118, &[]),
            sensor("TM1P", "内存扩展板A出风", "Other", 119, &[]),
            sensor("TM2P", "内存扩展板A稳压区", "Other", 120, &[]),
            sensor("TM3P", "内存环境3", "Other", 121, &[]),
            sensor("TM8P", "内存扩展板B进风", "Other", 122, &[]),
            sensor("TM9P", "内存扩展板B出风", "Other", 123, &[]),
            sensor("TM9P", "内存扩展板B稳压区", "Other", 124, &[]),
            sensor("TM0S", "内存扩展板A插槽1", "Other", 125, &[]),
            sensor("TM1S", "内存扩展板A插槽2", "Other", 126, &[]),
            sensor("TS0C", "扩展插槽", "Other", 127, &[]),
        ],
        aggregates: &[
        ],
    },
    Rule {
        scope: "MacPro4,1;MacPro5,1",
        sensors: &[
            sensor("TCAC", "CPU A核心参考", "CPU", 130, &[]),
            sensor("TCAH", "CPU A散热器", "CPU", 131, &[]),
            sensor("TCAD", "CPU A二极管", "CPU", 132, &[]),
            sensor("TCBC", "CPU B核心参考", "CPU", 133, &[]),
            sensor("TCBH", "CPU B散热器", "CPU", 134, &[]),
            sensor("TCBD", "CPU B二极管", "CPU", 135, &[]),
            sensor("TH1P", "硬盘位0", "Other", 136, &[]),
            sensor("TH2P", "硬盘位1", "Other", 137, &[]),
            sensor("TH3P", "硬盘位2", "Other", 138, &[]),
            sensor("TH4P", "硬盘位3", "Other", 139, &[]),
            sensor("TM1P", "内存邻近1", "Other", 140, &[]),
            sensor("TM2P", "内存邻近2", "Other", 141, &[]),
            sensor("TM3P", "内存邻近3", "Other", 142, &[]),
            sensor("TM4P", "内存邻近4", "Other", 143, &[]),
            sensor("TM5P", "内存邻近5", "Other", 144, &[]),
            sensor("TM6P", "内存邻近6", "Other", 145, &[]),
            sensor("TM7P", "内存邻近7", "Other", 146, &[]),
            sensor("TM8P", "内存邻近8", "Other", 147, &[]),
            sensor("Te1P", "PCIe环境", "Other", 148, &[]),
            sensor("Te1S", "PCIe插槽1", "Other", 149, &[]),
            sensor("Te2S", "PCIe插槽2", "Other", 150, &[]),
            sensor("Te3S", "PCIe插槽3", "Other", 151, &[]),
            sensor("Te4S", "PCIe插槽4", "Other", 152, &[]),
            sensor("Te5S", "PCIe RAID卡", "Other", 153, &[]),
            sensor("TN0D", "北桥二极管", "Other", 154, &[]),
            sensor("TN0H", "北桥散热器", "Other", 155, &[]),
        ],
        aggregates: &[
        ],
    },
    Rule {
        scope: "MacPro1,1;MacPro2,1;MacPro3,1;MacPro4,1;MacPro5,1",
        sensors: &[
            sensor("Tp1C", "电源AC/DC通道2", "Other", 158, &[]),
        ],
        aggregates: &[
        ],
    },
    Rule {
        scope: "MacPro6,1",
        sensors: &[
            sensor("TA0p", "主板环境", "Other", 161, &[]),
            sensor("TA2p", "环境3", "Other", 162, &[]),
            SensorFact { exclude: true, ..sensor("TC0p", "CPU局部邻近", "CPU", 163, &[]) },
            sensor("TC1p", "CPU核心供电邻近", "CPU", 164, &[]),
            sensor("Te0t", "PCIe交换芯片二极管", "Other", 165, &[]),
            sensor("TG0D", "GPU1二极管", "GPU", 166, &[]),
            sensor("TG1D", "GPU2二极管", "GPU", 167, &[]),
            sensor("TM0p", "内存1/2邻近", "Other", 168, &[]),
            sensor("TM1p", "内存3/4邻近", "Other", 169, &[]),
            SensorFact { exclude: true, ..sensor("Tm0p", "主板邻近", "Other", 170, &[]) },
            sensor("Tp0T", "电源次级散热器", "Other", 171, &[]),
            sensor("TI0P", "接口板邻近", "Other", 172, &[]),
            sensor("TC0C", "CPU传感器1", "CPU", 173, &[]),
            sensor("TC1C", "CPU传感器2", "CPU", 174, &[]),
            sensor("TC2C", "CPU传感器3", "CPU", 175, &[]),
            sensor("TC3C", "CPU传感器4", "CPU", 176, &[]),
            SensorFact { exclude: true, ..sensor("TG0p", "GPU1局部", "GPU", 177, &[]) },
            SensorFact { exclude: true, ..sensor("TG1p", "GPU2局部", "GPU", 178, &[]) },
        ],
        aggregates: &[
        ],
    },
    Rule {
        scope: "iMac4,1;iMac5,1",
        sensors: &[
            sensor("Tm0P", "时钟芯片邻近", "Other", 181, &[]),
        ],
        aggregates: &[
        ],
    },
    Rule {
        scope: "iMac9,1",
        sensors: &[
            sensor("TN0D", "北桥二极管", "Other", 184, &[]),
            sensor("TN0H", "北桥散热器", "Other", 185, &[]),
            sensor("Tm0P", "主板邻近", "Other", 186, &[]),
        ],
        aggregates: &[
        ],
    },
    Rule {
        scope: "iMac4,1;iMac5,1;iMac6,1;iMac7,1;iMac8,1;iMac9,1;MacMini3,1",
        sensors: &[
            sensor("TH0P", "硬盘邻近", "Other", 189, &[]),
        ],
        aggregates: &[
        ],
    },
    Rule {
        scope: "iMac15,1;iMac17,1",
        sensors: &[
            sensor("TG0p", "GPU邻近1", "GPU", 192, &[]),
            sensor("TG1p", "GPU邻近2", "GPU", 193, &[]),
            sensor("TG2p", "GPU邻近3", "GPU", 194, &[]),
            sensor("TG3p", "GPU邻近4", "GPU", 195, &[]),
        ],
        aggregates: &[
        ],
    },
    Rule {
        scope: "iMac13,1;iMac13,2",
        sensors: &[
            sensor("TH1B", "SSD温度", "Other", 198, &[]),
        ],
        aggregates: &[
        ],
    },
    Rule {
        scope: "iMac15,1;iMac17,1",
        sensors: &[
            sensor("TH1A", "SSD温度", "Other", 201, &[]),
        ],
        aggregates: &[
        ],
    },
    Rule {
        scope: "MacBook8,1+;MacBookAir6,2+;MacBookPro10,1;MacBookPro11,1;MacBookPro11,3;MacBookPro11,4;MacBookPro12,1;MacBookPro13,1;MacBookPro13,2;MacBookPro13,3;MacBookPro14,1;MacBookPro14,2;MacBookPro14,3;MacBookPro15,1;MacBookPro15,2",
        sensors: &[
            sensor("TH0A", "SSD温度", "Other", 204, &[]),
        ],
        aggregates: &[
        ],
    },
    Rule {
        scope: "iMacPro1,1",
        sensors: &[
            sensor("TH0A", "SSD A", "Other", 207, &[]),
            sensor("TH1A", "SSD B", "Other", 208, &[]),
            sensor("TG1P", "GPU邻近1", "GPU", 209, &[]),
            sensor("TG2P", "GPU邻近2", "GPU", 210, &[]),
            sensor("TG3P", "GPU邻近3", "GPU", 211, &[]),
        ],
        aggregates: &[
        ],
    },
 ];

/// Intel catalogue metadata only. Common CPU entries are possible keys, not a
/// ten-core hardware claim. Build measured averages from discovered physical keys.
pub fn catalogue(model: &str) -> Vec<Descriptor> {
    resolve_rules_with_source(SOURCE, normalize_model(model), RULES, keys_match)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{catalogue_for_architecture, keys_match_for_architecture, Classification};
    use std::collections::HashSet;

    fn get(model: &str, key: &str) -> Descriptor {
        catalogue(model)
            .into_iter()
            .find(|d| d.id == key)
            .unwrap_or_else(|| panic!("missing {model}/{key}"))
    }

    #[test]
    fn source_counts_include_empty_block_exclusions_and_zero_aliases() {
        assert_eq!(RULES.len(), 27);
        assert_eq!(RULES.iter().map(|r| r.sensors.len()).sum::<usize>(), 153);
        assert_eq!(
            RULES
                .iter()
                .flat_map(|r| r.sensors)
                .filter(|s| s.exclude)
                .count(),
            6
        );
        assert_eq!(
            RULES
                .iter()
                .flat_map(|r| r.sensors)
                .filter(|s| !s.exclude)
                .count(),
            147
        );
        assert_eq!(RULES.iter().map(|r| r.aggregates.len()).sum::<usize>(), 2);
        assert_eq!(
            RULES
                .iter()
                .flat_map(|r| r.sensors)
                .map(|s| s.aliases.len())
                .sum::<usize>(),
            0
        );
        assert_eq!(RULES.iter().filter(|r| r.sensors.is_empty()).count(), 1);
        assert_eq!(
            RULES
                .iter()
                .map(|r| r.scope.split(';').filter(|t| t.ends_with('+')).count())
                .sum::<usize>(),
            5
        );
    }

    #[test]
    fn unknown_intel_gets_only_explicit_common_candidate_metadata() {
        let common = catalogue("Unknown99,1");
        assert_eq!(common.len(), RULES[0].sensors.len() + 1);
        let cpu = get("Unknown99,1", "avg:cpu");
        assert_eq!(
            cpu.member_ids,
            ["TC0c", "TC1c", "TC2c", "TC3c", "TC4c", "TC5c", "TC6c", "TC7c", "TC8c", "TC9c"]
        );
        assert!(get("Unknown99,1", "TC0c").aliases.is_empty());
        assert_eq!(get("Unknown99,1", "TB0T").display_name, "电池最高温");
        assert!(!common.iter().any(|d| d.id == "TH0A"));
        assert!(common
            .iter()
            .all(|d| d.evidence.iter().all(|e| e.model_scope == "all")));
    }

    #[test]
    fn exact_blocks_override_and_exclusions_clear_group_membership() {
        let model = "MacBookPro5,1";
        assert_eq!(get(model, "TN0D").display_name, "CPU核心供电区");
        assert_eq!(get(model, "TN0D").group, "CPU");
        assert_eq!(get(model, "TN0P").display_name, "MCP邻近");
        assert_eq!(get(model, "TG0H").display_name, "GPU模拟二极管");
        assert_eq!(get(model, "TG0H").evidence[0].line, 83);
        let cpu = get("MacBookPro8,3", "avg:cpu");
        assert_eq!(cpu.member_ids.len(), 9);
        assert!(!cpu.member_ids.contains(&"TC4c".into()));
        assert!(!catalogue("MacBookPro8,3").iter().any(|d| d.id == "TC4c"));
        let pro = catalogue("MacPro6,1");
        for removed in ["TC0P", "TC0p", "Tm0p", "TG0P", "TG0p", "TG1P", "TG1p"] {
            assert!(!pro.iter().any(|d| d.id == removed), "{removed}");
        }
        assert!(pro.iter().any(|d| d.id == "TM0p")); // prefix case is significant
        assert_eq!(get("MacPro6,1", "avg:cpu").member_ids.len(), 6);
        assert_eq!(get("Macmini1,1", "TC0P").display_name, "CPU稳压区");
    }

    #[test]
    fn final_character_case_equivalence_is_metadata_only_and_architecture_specific() {
        assert!(keys_match("TC1c", "TC1C"));
        for (a, b) in [
            ("TC1c", "Tc1C"),
            ("TC1c", "tc1c"),
            ("TC1c", "TC2c"),
            ("TC1", "TC1"),
            ("TC1c0", "TC1C0"),
            ("TCé", "TCÉ"),
        ] {
            assert!(!keys_match(a, b), "{a}/{b}");
        }
        assert!(keys_match_for_architecture("x86_64", "TC1C", "TC1c"));
        assert!(!keys_match_for_architecture("aarch64", "TC1C", "TC1c"));
        assert!(keys_match_for_architecture("arm64", "TW0P", "TW0P"));
        assert!(!keys_match_for_architecture("unknown", "TC1c", "TC1c"));
        let c = catalogue("MacBookPro13,2");
        assert!(!c.iter().any(|d| d.id == "TC0c" || d.id == "TC1c"));
        assert!(c.iter().any(|d| d.id == "TC1C"));
        let cpu = get("MacBookPro13,2", "avg:cpu");
        assert_eq!(cpu.member_ids.len(), 9); // remaining TC9c plus eight replacements
        assert_eq!(cpu.member_ids[0], "TC9c");
        assert_eq!(cpu.evidence.len(), 2); // both source group declarations survive
        assert!(!get("MacPro1,1", "avg:cpu")
            .member_ids
            .contains(&"TC0C".into())); // later node is ungrouped
    }

    #[test]
    fn ranges_include_lower_bounds_and_do_not_cross_families() {
        assert!(catalogue("MacBook8,1").iter().any(|d| d.id == "TH0A"));
        assert!(!catalogue("MacBook7,1").iter().any(|d| d.id == "TH0A"));
        assert!(catalogue("MacBookAir6,2").iter().any(|d| d.id == "TH0A"));
        assert!(!catalogue("MacBookAir6,1").iter().any(|d| d.id == "TH0A"));
        assert!(catalogue("MacBookPro13,2").iter().any(|d| d.id == "TC1C"));
        assert!(!catalogue("MacBookPro13,1").iter().any(|d| d.id == "TC1C"));
        assert!(catalogue("MacBookAir13,2").iter().any(|d| d.id == "TC8C")); // own Air range still applies
        assert!(!catalogue("iMac13,2").iter().any(|d| d.id == "TC1C"));
        for malformed in [
            "MacBookPro013,2",
            "MacBookPro13,2 ",
            "MacBookPro6553,6",
            "macbookpro13,2",
        ] {
            assert!(!catalogue(malformed).iter().any(|d| d.id == "TC1C"));
        }
    }

    #[test]
    fn duplicate_source_nodes_resolve_in_document_order_without_inventing_aliases() {
        let d = get("MacPro1,1", "TM9P");
        assert_eq!(d.display_name, "内存扩展板B稳压区");
        assert_eq!(d.evidence[0].line, 124);
        assert!(d.aliases.is_empty());
        assert_eq!(
            catalogue("MacPro1,1")
                .iter()
                .filter(|d| d.id == "TM9P")
                .count(),
            1
        );
        assert_eq!(get("Macmini3,1", "TH0P").display_name, "硬盘外壳");
        assert_eq!(get("MacMini3,1", "TH0P").display_name, "硬盘邻近"); // literal source case retained
    }

    #[test]
    fn architecture_routing_keeps_both_catalogues_isolated() {
        for arch in ["x86_64", "x86", "i386"] {
            let intel = catalogue_for_architecture(arch, "Unknown99,1");
            assert_eq!(intel, catalogue("Unknown99,1"));
            assert!(intel.iter().any(|d| d.id == "TC0c"));
            assert!(!intel.iter().any(|d| d.id == "TPMP"));
        }
        for arch in ["arm64", "aarch64"] {
            let arm = catalogue_for_architecture(arch, "Mac16,8");
            assert_eq!(arm, crate::catalog::catalogue("Mac16,8"));
            assert!(arm.iter().any(|d| d.id == "Tp01"));
            assert!(!arm.iter().any(|d| d.id == "TC0c"));
            let neo = catalogue_for_architecture(arch, "MacBookNeo1,1");
            assert!(neo.iter().any(|d| d.id == "TW0P"));
            assert!(neo.iter().any(|d| d.id == "TW0p")); // ARM comparator is exact
        }
        for arch in ["", "ARM64", "powerpc", "amd64"] {
            assert!(catalogue_for_architecture(arch, "Mac16,8").is_empty());
        }
    }

    #[test]
    fn resolved_catalogues_have_unique_sources_and_nonduplicated_average_members() {
        for model in RULES
            .iter()
            .flat_map(|r| r.scope.split(';'))
            .map(|m| m.trim_end_matches('+'))
        {
            let c = catalogue(model);
            let mut ids = HashSet::new();
            for d in &c {
                assert!(ids.insert(&d.id), "{model}/{}", d.id);
                assert_eq!(d.classification, Classification::ProvisionalMapped);
                assert!(d.evidence.iter().all(|e| e.file == SOURCE && e.line > 0));
                assert!(d.aliases.is_empty());
                if d.is_aggregate() {
                    let unique: HashSet<_> = d.member_ids.iter().collect();
                    assert_eq!(unique.len(), d.member_ids.len());
                    for member in &d.member_ids {
                        assert_eq!(
                            c.iter()
                                .filter(|source| source.id == *member && !source.is_aggregate())
                                .count(),
                            1
                        );
                    }
                    for (i, member) in d.member_ids.iter().enumerate() {
                        assert!(d.member_ids[i + 1..]
                            .iter()
                            .all(|other| !keys_match(member, other)));
                    }
                } else {
                    assert_eq!(d.id.len(), 4);
                }
            }
        }
    }
}
