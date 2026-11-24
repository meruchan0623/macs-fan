//! Exact Product facts, independently labelled. No vendor XML is embedded.
//! Product names identify a provider observation, not a calibrated die location.
pub const SOURCE: &str = "evidence/deep/resources/IOKitSensors.xml";

pub struct Fact {
    pub product: &'static str,
    pub name: &'static str,
    pub group: &'static str,
    pub line: u16,
    pub average: Option<&'static str>,
}

pub struct Average {
    pub id: &'static str,
    pub name: &'static str,
    pub group: &'static str,
    pub line: u16,
}

const fn fact(
    product: &'static str,
    name: &'static str,
    group: &'static str,
    line: u16,
    average: Option<&'static str>,
) -> Fact {
    Fact {
        product,
        name,
        group,
        line,
        average,
    }
}

pub const FACTS: &[Fact] = &[
    fact(
        "GPU MTR Temp Sensor1",
        "GPU传感器1",
        "GPU",
        5,
        Some("hid:avg:gpu"),
    ),
    fact(
        "GPU MTR Temp Sensor4",
        "GPU传感器2",
        "GPU",
        6,
        Some("hid:avg:gpu"),
    ),
    fact("gas gauge battery", "电池电量计", "Battery", 8, None),
    fact("NAND CH0 temp", "SSD", "Other", 9, None),
    fact("SOC MTR Temp Sensor0", "SoC参考1", "Other", 10, None),
    fact("SOC MTR Temp Sensor1", "SoC参考2", "Other", 11, None),
    fact("SOC MTR Temp Sensor2", "SoC参考3", "Other", 12, None),
    fact(
        "eACC MTR Temp Sensor0",
        "CPU能效簇1",
        "CPU",
        14,
        Some("hid:avg:efficiency"),
    ),
    fact(
        "eACC MTR Temp Sensor3",
        "CPU能效簇2",
        "CPU",
        15,
        Some("hid:avg:efficiency"),
    ),
    fact(
        "pACC MTR Temp Sensor2",
        "CPU性能簇1",
        "CPU",
        18,
        Some("hid:avg:performance"),
    ),
    fact(
        "pACC MTR Temp Sensor3",
        "CPU性能簇2",
        "CPU",
        19,
        Some("hid:avg:performance"),
    ),
    fact(
        "pACC MTR Temp Sensor4",
        "CPU性能簇3",
        "CPU",
        20,
        Some("hid:avg:performance"),
    ),
    fact(
        "pACC MTR Temp Sensor5",
        "CPU性能簇4",
        "CPU",
        21,
        Some("hid:avg:performance"),
    ),
    fact(
        "pACC MTR Temp Sensor7",
        "CPU性能簇5",
        "CPU",
        22,
        Some("hid:avg:performance"),
    ),
    fact(
        "pACC MTR Temp Sensor8",
        "CPU性能簇6",
        "CPU",
        23,
        Some("hid:avg:performance"),
    ),
    fact(
        "pACC MTR Temp Sensor9",
        "CPU性能簇7",
        "CPU",
        24,
        Some("hid:avg:performance"),
    ),
    fact(
        "ANE MTR Temp Sensor1",
        "神经网络引擎参考",
        "Other",
        26,
        None,
    ),
    fact("ISP MTR Temp Sensor5", "图像处理器参考", "Other", 27, None),
    fact(
        "PMGR SOC Die Temp Sensor0",
        "SoC电源管理1",
        "Other",
        29,
        Some("hid:avg:power-soc"),
    ),
    fact(
        "PMGR SOC Die Temp Sensor1",
        "SoC电源管理2",
        "Other",
        30,
        Some("hid:avg:power-soc"),
    ),
    fact(
        "PMGR SOC Die Temp Sensor2",
        "SoC电源管理3",
        "Other",
        31,
        Some("hid:avg:power-soc"),
    ),
    fact("PMU tdie1", "电源管理1", "Other", 34, Some("hid:avg:power")),
    fact("PMU tdie2", "电源管理2", "Other", 35, Some("hid:avg:power")),
    fact("PMU tdie4", "电源管理4", "Other", 36, Some("hid:avg:power")),
    fact("PMU tdie5", "电源管理5", "Other", 37, Some("hid:avg:power")),
    fact("PMU tdie6", "电源管理6", "Other", 38, Some("hid:avg:power")),
    fact("PMU tdie7", "电源管理7", "Other", 39, Some("hid:avg:power")),
    fact("PMU tdie8", "电源管理8", "Other", 40, Some("hid:avg:power")),
    fact("Charger TQ0d", "充电器邻近", "Other", 42, None),
];

pub const AVERAGES: &[Average] = &[
    Average {
        id: "hid:avg:gpu",
        name: "GPU邻近均值",
        group: "GPU",
        line: 4,
    },
    Average {
        id: "hid:avg:efficiency",
        name: "CPU能效簇均值",
        group: "CPU",
        line: 13,
    },
    Average {
        id: "hid:avg:performance",
        name: "CPU性能簇均值",
        group: "CPU",
        line: 17,
    },
    Average {
        id: "hid:avg:power-soc",
        name: "SoC电源管理均值",
        group: "Other",
        line: 28,
    },
    Average {
        id: "hid:avg:power",
        name: "电源管理均值",
        group: "Other",
        line: 33,
    },
];

pub fn lookup(product: &str) -> Option<&'static Fact> {
    FACTS.iter().find(|fact| fact.product == product)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    #[test]
    fn exact_products_and_group_members_are_unambiguous() {
        assert_eq!(FACTS.len(), 29);
        assert_eq!(AVERAGES.len(), 5);
        let mut products = HashSet::new();
        for fact in FACTS {
            assert!(products.insert(fact.product));
            if let Some(group) = fact.average {
                assert!(AVERAGES.iter().any(|average| average.id == group));
            }
        }
        for average in AVERAGES {
            assert!(FACTS.iter().any(|fact| fact.average == Some(average.id)));
        }
        assert!(lookup("GPU MTR Temp Sensor1").is_some());
        assert!(lookup("GPU MTR Temp Sensor1 ").is_none());
        assert!(lookup("gpu MTR Temp Sensor1").is_none());
        assert!(lookup("unknown").is_none());
    }
}
