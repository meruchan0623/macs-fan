//! Provisional sensor descriptions from locally verified key/model facts.
//! No vendor XML or vendor implementation is embedded. Chinese labels are ours.
//! See ../../docs/catalog-evidence.md for provenance and implementation boundaries.
//! A descriptor is metadata, never evidence that a key exists or a temperature is valid.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Classification {
    ProvisionalMapped,
    Raw,
}

impl Classification {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProvisionalMapped => "provisional_mapped",
            Self::Raw => "raw",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Evidence {
    /// Path relative to outputs/, retained as an evidence reference, not loaded at runtime.
    pub file: &'static str,
    pub line: u16,
    /// Model scope, "all", or an explicit non-model-specific reference limitation.
    pub model_scope: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Descriptor {
    /// Real SMC key, or an application-owned aggregate ID beginning with "avg:".
    pub id: String,
    pub display_name: String,
    /// Alternative labels for this same source key, never independent readings.
    pub aliases: Vec<String>,
    /// Source group requests hiding its members. UI presentation remains a caller policy.
    pub hide_sensors: bool,
    pub group: &'static str,
    pub classification: Classification,
    /// Only aggregates have members. Entries are distinct real keys, never aliases.
    pub member_ids: Vec<String>,
    pub evidence: Vec<Evidence>,
    pub note: Option<&'static str>,
}

impl Descriptor {
    pub fn is_aggregate(&self) -> bool {
        !self.member_ids.is_empty()
    }
}

const ARM_SOURCE: &str = "evidence/deep/resources/smcSensorsArm.xml";
const ALIAS_SOURCE: &str = "evidence/deep/resources/idSwapRules.xml";
const REFERENCE_SOURCE: &str = "evidence/deep/pseudocode/low-value-correction.c";
const REFERENCE_SCOPE: &str = "not_model_specific; gated_by_exact_arm_catalogue";
const REFERENCE_NOTE: &str = "静态低值回退函数将此键作为Tp核心读数的参考；未证实具体部件或芯片结温，不用于补值或修正原始读数";
const ALIAS_NOTE: &str = "别名共享此SMC键的同一读数；核心标签尚未按本机CPU数量过滤";
const AGGREGATE_NOTE: &str = "资源明确声明的成员集合；目录不计算温度，也不保证成员键存在或有效";

// Facts are (source model, normalized model, idSwapRules.xml evidence line).
const ALIASES: &[(&str, &str, u16)] = &[
    ("Mac13,1", "MacStudio1,1", 3),
    ("Mac13,2", "MacStudio1,2", 4),
    ("Mac14,3", "Macmini10,1", 5),
    ("Mac14,12", "Macmini10,2", 6),
    ("Mac14,7", "MacBookPro17,2", 7),
    ("Mac14,5", "MacBookPro19,3", 8),
    ("Mac14,6", "MacBookPro19,4", 9),
    ("Mac14,9", "MacBookPro19,1", 10),
    ("Mac14,10", "MacBookPro19,2", 11),
    ("Mac14,8", "MacPro8,1", 12),
    ("Mac14,13", "MacStudio2,1", 13),
    ("Mac14,14", "MacStudio2,2", 14),
    ("Mac14,2", "MacBookAir11,1", 15),
    ("Mac14,15", "MacBookAir11,2", 16),
    ("Mac15,3", "MacBookPro20,1", 17),
    ("Mac15,6", "MacBookPro20,2", 18),
    ("Mac15,8", "MacBookPro20,3", 19),
    ("Mac15,10", "MacBookPro20,4", 20),
    ("Mac15,7", "MacBookPro20,5", 21),
    ("Mac15,11", "MacBookPro20,6", 22),
    ("Mac15,9", "MacBookPro20,7", 23),
    ("Mac15,4", "iMac22,1", 24),
    ("Mac15,5", "iMac22,2", 25),
    ("Mac15,2", "MacBookAir12,1", 26),
    ("Mac15,13", "MacBookAir12,2", 27),
    ("Mac15,14", "MacStudio3,2", 28),
    ("Mac16,10", "Macmini11,1", 29),
    ("Mac16,11", "Macmini11,2", 30),
    ("Mac16,12", "MacBookAir13,1", 31),
    ("Mac16,13", "MacBookAir13,2", 32),
    ("Mac16,3", "iMac23,1", 33),
    ("Mac16,2", "iMac23,2", 34),
    ("Mac16,1", "MacBookPro21,1", 35),
    ("Mac16,8", "MacBookPro21,2", 36),
    ("Mac16,6", "MacBookPro21,3", 37),
    ("Mac16,7", "MacBookPro21,4", 38),
    ("Mac16,5", "MacBookPro21,5", 39),
    ("Mac16,9", "MacStudio3,1", 40),
    ("Mac17,2", "MacBookPro22,1", 41),
    ("Mac17,9", "MacBookPro22,2", 42),
    ("Mac17,7", "MacBookPro22,3", 43),
    ("Mac17,8", "MacBookPro22,4", 44),
    ("Mac17,6", "MacBookPro22,5", 45),
    ("Mac17,3", "MacBookAir14,1", 46),
    ("Mac17,4", "MacBookAir14,2", 47),
    ("Mac17,5", "MacBookNeo1,1", 48),
];

/// Apply only verified full-string aliases. Unknown input, case and whitespace are preserved.
pub fn normalize_model(model: &str) -> &str {
    ALIASES
        .iter()
        .find(|(source, _, _)| *source == model)
        .map_or(model, |(_, target, _)| *target)
}

pub fn normalization_evidence(model: &str) -> Option<Evidence> {
    ALIASES
        .iter()
        .find(|(source, _, _)| *source == model)
        .map(|(source, _, line)| Evidence {
            file: ALIAS_SOURCE,
            line: *line,
            model_scope: source,
        })
}

// ARM-only facts in source document order. This is metadata, not a hardware allowlist.
pub(crate) struct SensorFact {
    pub(crate) key: &'static str,
    pub(crate) name: &'static str,
    pub(crate) group: &'static str,
    pub(crate) line: u16,
    pub(crate) aliases: &'static [&'static str],
    pub(crate) exclude: bool,
}

pub(crate) const fn sensor(
    key: &'static str,
    name: &'static str,
    group: &'static str,
    line: u16,
    aliases: &'static [&'static str],
) -> SensorFact {
    SensorFact {
        key,
        name,
        group,
        line,
        aliases,
        exclude: false,
    }
}

pub(crate) struct AggregateFact {
    pub(crate) id: &'static str,
    pub(crate) name: &'static str,
    pub(crate) group: &'static str,
    pub(crate) line: u16,
    pub(crate) hide_sensors: bool,
    pub(crate) members: &'static [&'static str],
}

pub(crate) struct Rule {
    pub(crate) scope: &'static str,
    pub(crate) sensors: &'static [SensorFact],
    pub(crate) aggregates: &'static [AggregateFact],
}

const COMMON: Rule = Rule {
    scope: "all",
    sensors: &[
        sensor("TA0P", "环境", "Other", 4, &[]),
        sensor("TB0T", "电池", "Battery", 5, &[]),
        sensor("Th1H", "散热器邻近", "Other", 6, &[]),
        sensor("TPMP", "电源邻近", "Other", 7, &[]),
        sensor("TW0P", "无线模块邻近", "Other", 8, &[]),
    ],
    aggregates: &[],
};

const RULES: &[Rule] = &[
    Rule {
        scope: "MacBookPro17,1+;MacBookAir10,1+",
        sensors: &[
            sensor("Ts0P", "触控板", "Other", 11, &[]),
            sensor("Ts1P", "触控板执行器", "Other", 12, &[]),
        ],
        aggregates: &[],
    },
    Rule {
        scope: "iMac21,1+",
        sensors: &[
            sensor("TL0p", "显示屏邻近1", "Other", 15, &[]),
            sensor("TL1P", "显示屏邻近2", "Other", 16, &[]),
        ],
        aggregates: &[],
    },
    Rule {
        scope: "Macmini9,1",
        sensors: &[sensor("TIOP", "雷雳接口邻近", "Other", 19, &[])],
        aggregates: &[],
    },
    Rule {
        scope:
            "MacBookPro18,1;MacBookPro18,2;MacBookPro18,3;MacBookPro18,4;MacStudio1,1;MacStudio1,2",
        sensors: &[
            sensor("Tg05", "GPU簇1", "GPU", 23, &[]),
            sensor("Tg0D", "GPU簇2", "GPU", 24, &[]),
            sensor("Tg0L", "GPU簇3", "GPU", 25, &[]),
            sensor("Tg0T", "GPU簇4", "GPU", 26, &[]),
            sensor("Tg0b", "GPU簇5", "GPU", 27, &[]),
            sensor("Tg0j", "GPU簇6", "GPU", 28, &[]),
            sensor("Tg0r", "GPU簇7", "GPU", 29, &[]),
            sensor("Tg0z", "GPU簇8", "GPU", 30, &[]),
            sensor("Tp01", "CPU性能核心1", "CPU", 33, &[]),
            sensor("Tp05", "CPU性能核心2", "CPU", 34, &[]),
            sensor("Tp0D", "CPU性能核心3", "CPU", 35, &[]),
            sensor("Tp0H", "CPU性能核心4", "CPU", 36, &[]),
            sensor("Tp0L", "CPU性能核心5", "CPU", 37, &[]),
            sensor("Tp0P", "CPU性能核心6", "CPU", 38, &[]),
            sensor("Tp0X", "CPU性能核心7", "CPU", 39, &[]),
            sensor("Tp0b", "CPU性能核心8", "CPU", 40, &[]),
            sensor("Tp09", "CPU能效核心1", "CPU", 41, &[]),
            sensor("Tp0T", "CPU能效核心2", "CPU", 42, &[]),
            sensor("Tp21", "CPU性能核心9", "CPU", 43, &[]),
            sensor("Tp25", "CPU性能核心10", "CPU", 44, &[]),
            sensor("Tp2D", "CPU性能核心11", "CPU", 45, &[]),
            sensor("Tp2H", "CPU性能核心12", "CPU", 46, &[]),
            sensor("Tp2L", "CPU性能核心13", "CPU", 47, &[]),
            sensor("Tp2P", "CPU性能核心14", "CPU", 48, &[]),
            sensor("Tp2X", "CPU性能核心15", "CPU", 49, &[]),
            sensor("Tp2b", "CPU性能核心16", "CPU", 50, &[]),
            sensor("Tp29", "CPU能效核心3", "CPU", 51, &[]),
            sensor("Tp2T", "CPU能效核心4", "CPU", 52, &[]),
            sensor("Tm00", "内存组1", "Other", 54, &[]),
            sensor("Tm04", "内存组2", "Other", 55, &[]),
            sensor("Tm08", "内存组3", "Other", 56, &[]),
            sensor("Tm0C", "内存组4", "Other", 57, &[]),
        ],
        aggregates: &[
            AggregateFact {
                id: "avg:gpu",
                name: "GPU簇均值",
                group: "GPU",
                line: 22,
                hide_sensors: false,
                members: &[
                    "Tg05", "Tg0D", "Tg0L", "Tg0T", "Tg0b", "Tg0j", "Tg0r", "Tg0z",
                ],
            },
            AggregateFact {
                id: "avg:cpu",
                name: "CPU核心均值",
                group: "CPU",
                line: 32,
                hide_sensors: false,
                members: &[
                    "Tp01", "Tp05", "Tp0D", "Tp0H", "Tp0L", "Tp0P", "Tp0X", "Tp0b", "Tp09", "Tp0T",
                    "Tp21", "Tp25", "Tp2D", "Tp2H", "Tp2L", "Tp2P", "Tp2X", "Tp2b", "Tp29", "Tp2T",
                ],
            },
        ],
    },
    Rule {
        scope: "MacStudio1,1+",
        sensors: &[
            sensor("TaFL", "前左气流", "Other", 60, &[]),
            sensor("TaFR", "前右气流", "Other", 61, &[]),
            sensor("TaRL", "后左气流", "Other", 62, &[]),
            sensor("TaRR", "后右气流", "Other", 63, &[]),
        ],
        aggregates: &[],
    },
    Rule {
        scope: "MacStudio2,1;MacStudio2,2",
        sensors: &[
            sensor("Tg0f", "GPU簇1", "GPU", 67, &[]),
            sensor("Tg0j", "GPU簇2", "GPU", 68, &[]),
            sensor("Tg1h", "GPU簇3", "GPU", 69, &[]),
            sensor("Tg1l", "GPU簇4", "GPU", 70, &[]),
            sensor("Tg2f", "GPU簇5", "GPU", 71, &[]),
            sensor("Tp01", "CPU性能核心1", "CPU", 74, &[]),
            sensor("Tp09", "CPU性能核心2", "CPU", 75, &[]),
            sensor("Tp0X", "CPU性能核心3", "CPU", 76, &[]),
            sensor("Tp0f", "CPU性能核心4", "CPU", 77, &[]),
            sensor("Tp1B", "CPU性能核心5", "CPU", 78, &[]),
            sensor("Tp1J", "CPU性能核心6", "CPU", 79, &[]),
            sensor("Tp1h", "CPU性能核心7", "CPU", 80, &[]),
            sensor("Tp1p", "CPU性能核心8", "CPU", 81, &[]),
            sensor("Tp05", "CPU能效核心1", "CPU", 82, &[]),
            sensor("Tp0b", "CPU能效核心2", "CPU", 83, &[]),
            sensor("Tp0D", "CPU能效核心3", "CPU", 84, &[]),
            sensor("Tp0j", "CPU能效核心4", "CPU", 85, &[]),
        ],
        aggregates: &[
            AggregateFact {
                id: "avg:gpu",
                name: "GPU簇均值",
                group: "GPU",
                line: 66,
                hide_sensors: false,
                members: &["Tg0f", "Tg0j", "Tg1h", "Tg1l", "Tg2f"],
            },
            AggregateFact {
                id: "avg:cpu",
                name: "CPU核心均值",
                group: "CPU",
                line: 73,
                hide_sensors: false,
                members: &[
                    "Tp01", "Tp09", "Tp0X", "Tp0f", "Tp1B", "Tp1J", "Tp1h", "Tp1p", "Tp05", "Tp0b",
                    "Tp0D", "Tp0j",
                ],
            },
        ],
    },
    Rule {
        scope: "MacBookPro17,2;MacBookAir11,1;MacBookAir11,2",
        sensors: &[
            sensor("Tg0f", "GPU簇1", "GPU", 89, &[]),
            sensor("Tg0n", "GPU簇2", "GPU", 90, &[]),
            sensor("Tp01", "CPU性能核心1", "CPU", 92, &[]),
            sensor("Tp09", "CPU性能核心2", "CPU", 93, &[]),
            sensor("Tp0f", "CPU性能核心3", "CPU", 94, &[]),
            sensor("Tp0n", "CPU性能核心4", "CPU", 95, &[]),
            sensor("Tp05", "CPU能效核心1", "CPU", 96, &[]),
            sensor("Tp0D", "CPU能效核心2", "CPU", 97, &[]),
            sensor("Tp0j", "CPU能效核心3", "CPU", 98, &[]),
            sensor("Tp0r", "CPU能效核心4", "CPU", 99, &[]),
        ],
        aggregates: &[AggregateFact {
            id: "avg:cpu",
            name: "CPU核心均值",
            group: "CPU",
            line: 91,
            hide_sensors: false,
            members: &[
                "Tp01", "Tp09", "Tp0f", "Tp0n", "Tp05", "Tp0D", "Tp0j", "Tp0r",
            ],
        }],
    },
    Rule {
        scope:
            "MacBookPro19,1;MacBookPro19,2;MacBookPro19,3;MacBookPro19,4;Macmini10,1;Macmini10,2",
        sensors: &[
            sensor("Tg0f", "GPU簇1", "GPU", 103, &[]),
            sensor("Tg1l", "GPU簇2", "GPU", 104, &[]),
            sensor("TaLP", "左侧气流", "Other", 105, &[]),
            sensor("TaRF", "右侧气流", "Other", 106, &[]),
            sensor("Tp01", "CPU性能核心1", "CPU", 108, &[]),
            sensor("Tp09", "CPU性能核心2", "CPU", 109, &[]),
            sensor("Tp0X", "CPU性能核心3", "CPU", 110, &[]),
            sensor("Tp0f", "CPU性能核心4", "CPU", 111, &[]),
            sensor("Tp1B", "CPU性能核心5", "CPU", 112, &[]),
            sensor("Tp1J", "CPU性能核心6", "CPU", 113, &[]),
            sensor("Tp1h", "CPU性能核心7", "CPU", 114, &[]),
            sensor("Tp1p", "CPU性能核心8", "CPU", 115, &[]),
            sensor("Tp05", "CPU能效核心1", "CPU", 116, &[]),
            sensor("Tp0b", "CPU能效核心2", "CPU", 117, &[]),
            sensor("Tp0D", "CPU能效核心3", "CPU", 118, &[]),
            sensor("Tp0j", "CPU能效核心4", "CPU", 119, &[]),
        ],
        aggregates: &[AggregateFact {
            id: "avg:cpu",
            name: "CPU核心均值",
            group: "CPU",
            line: 107,
            hide_sensors: false,
            members: &[
                "Tp01", "Tp09", "Tp0X", "Tp0f", "Tp1B", "Tp1J", "Tp1h", "Tp1p", "Tp05", "Tp0b",
                "Tp0D", "Tp0j",
            ],
        }],
    },
    Rule {
        scope: "Macmini10,1",
        sensors: &[sensor("Tp0n", "CPU性能核心3", "CPU", 123, &[])],
        aggregates: &[],
    },
    Rule {
        scope: "MacBookPro20,1;iMac22,1;iMac22,2;MacBookAir12,1;MacBookAir12,2",
        sensors: &[
            sensor("Tp01", "CPU性能核心1", "CPU", 127, &[]),
            sensor("Tp09", "CPU性能核心2", "CPU", 128, &[]),
            sensor("Tp0X", "CPU性能核心3", "CPU", 129, &[]),
            sensor("Tp0f", "CPU性能核心4", "CPU", 130, &[]),
            sensor("Tp05", "CPU能效核心1", "CPU", 131, &[]),
            sensor("Tp0b", "CPU能效核心2", "CPU", 132, &[]),
            sensor("Tp0D", "CPU能效核心3", "CPU", 133, &[]),
            sensor("Tp0j", "CPU能效核心4", "CPU", 134, &[]),
            sensor("Tg0D", "GPU核心组", "GPU", 137, &[]),
            sensor("Tg0P", "GPU核心组", "GPU", 138, &[]),
            sensor("Tg0X", "GPU核心组", "GPU", 139, &[]),
            sensor("Tg0b", "GPU核心组", "GPU", 140, &[]),
            sensor("Tg0j", "GPU核心组", "GPU", 141, &[]),
            sensor("Tg0v", "GPU核心组", "GPU", 142, &[]),
        ],
        aggregates: &[
            AggregateFact {
                id: "avg:cpu",
                name: "CPU核心均值",
                group: "CPU",
                line: 126,
                hide_sensors: false,
                members: &[
                    "Tp01", "Tp09", "Tp0X", "Tp0f", "Tp05", "Tp0b", "Tp0D", "Tp0j",
                ],
            },
            AggregateFact {
                id: "avg:gpu",
                name: "GPU核心均值",
                group: "GPU",
                line: 136,
                hide_sensors: true,
                members: &["Tg0D", "Tg0P", "Tg0X", "Tg0b", "Tg0j", "Tg0v"],
            },
        ],
    },
    Rule {
        scope: "MacBookPro20,2;MacBookPro20,5",
        sensors: &[
            sensor("Tp05", "CPU性能核心1", "CPU", 147, &[]),
            sensor("Tp0D", "CPU性能核心2", "CPU", 148, &[]),
            sensor("Tp0L", "CPU性能核心3", "CPU", 149, &[]),
            sensor("Tp0S", "CPU性能核心4", "CPU", 150, &[]),
            sensor("Tp17", "CPU性能核心5", "CPU", 151, &[]),
            sensor("Tp1F", "CPU性能核心6", "CPU", 152, &[]),
            sensor("Te05", "CPU能效核心1", "CPU", 153, &[]),
            sensor("Te0P", "CPU能效核心2", "CPU", 154, &[]),
            sensor("Te0H", "CPU能效核心3", "CPU", 155, &[]),
            sensor("Te04", "CPU能效核心4", "CPU", 156, &[]),
            sensor("Te0S", "CPU能效核心5", "CPU", 157, &[]),
            sensor("Te0U", "CPU能效核心6", "CPU", 158, &[]),
            sensor("Tg05", "GPU簇1", "GPU", 161, &[]),
            sensor("Tg0D", "GPU簇2", "GPU", 162, &[]),
            sensor("Tg0v", "GPU簇3", "GPU", 163, &[]),
            sensor("Tg1B", "GPU簇4", "GPU", 164, &[]),
        ],
        aggregates: &[
            AggregateFact {
                id: "avg:cpu",
                name: "CPU核心均值",
                group: "CPU",
                line: 146,
                hide_sensors: false,
                members: &[
                    "Tp05", "Tp0D", "Tp0L", "Tp0S", "Tp17", "Tp1F", "Te05", "Te0P", "Te0H", "Te04",
                    "Te0S", "Te0U",
                ],
            },
            AggregateFact {
                id: "avg:gpu",
                name: "GPU簇均值",
                group: "GPU",
                line: 160,
                hide_sensors: false,
                members: &["Tg05", "Tg0D", "Tg0v", "Tg1B"],
            },
        ],
    },
    Rule {
        scope: "MacBookPro20,3;MacBookPro20,4;MacBookPro20,6;MacBookPro20,7",
        sensors: &[
            sensor("Tp05", "CPU性能核心1", "CPU", 169, &[]),
            sensor("Tp0D", "CPU性能核心2", "CPU", 170, &[]),
            sensor("Tp0L", "CPU性能核心3", "CPU", 171, &[]),
            sensor("Tp1F", "CPU性能核心4", "CPU", 172, &[]),
            sensor("Tp17", "CPU性能核心5", "CPU", 173, &[]),
            sensor("Tp0S", "CPU性能核心6", "CPU", 174, &[]),
            sensor("Tp1h", "CPU性能核心7", "CPU", 175, &[]),
            sensor("Tp1p", "CPU性能核心8", "CPU", 176, &[]),
            sensor("Tp1y", "CPU性能核心9", "CPU", 177, &[]),
            sensor("Tp30", "CPU性能核心10", "CPU", 178, &[]),
            sensor("Tp26", "CPU性能核心11", "CPU", 179, &[]),
            sensor("Tp3s", "CPU性能核心12", "CPU", 180, &[]),
            sensor("Te05", "CPU能效核心1", "CPU", 181, &["CPU能效核心2"]),
            sensor("Te0L", "CPU能效核心3", "CPU", 182, &["CPU能效核心4"]),
            sensor("Tg05", "GPU簇1", "GPU", 185, &[]),
            sensor("Tg0z", "GPU簇2", "GPU", 186, &[]),
            sensor("Tg34", "GPU簇3", "GPU", 187, &[]),
            sensor("Tg3y", "GPU簇4", "GPU", 188, &[]),
        ],
        aggregates: &[
            AggregateFact {
                id: "avg:cpu",
                name: "CPU核心均值",
                group: "CPU",
                line: 168,
                hide_sensors: false,
                members: &[
                    "Tp05", "Tp0D", "Tp0L", "Tp1F", "Tp17", "Tp0S", "Tp1h", "Tp1p", "Tp1y", "Tp30",
                    "Tp26", "Tp3s", "Te05", "Te0L",
                ],
            },
            AggregateFact {
                id: "avg:gpu",
                name: "GPU簇均值",
                group: "GPU",
                line: 184,
                hide_sensors: false,
                members: &["Tg05", "Tg0z", "Tg34", "Tg3y"],
            },
        ],
    },
    Rule {
        scope: "Macmini11,1;MacBookPro21,1;iMac23,1;iMac23,2;MacBookAir13,1;MacBookAir13,2",
        sensors: &[
            sensor("Tp01", "CPU性能核心1", "CPU", 193, &[]),
            sensor("Tp05", "CPU性能核心2", "CPU", 194, &[]),
            sensor("Tp09", "CPU性能核心3", "CPU", 195, &[]),
            sensor("Tp0D", "CPU性能核心4", "CPU", 196, &[]),
            sensor("Te05", "CPU能效核心1", "CPU", 197, &[]),
            sensor("Te09", "CPU能效核心2", "CPU", 198, &[]),
            sensor("Te0H", "CPU能效核心3", "CPU", 199, &[]),
            sensor("Te0S", "CPU能效核心4", "CPU", 200, &[]),
            sensor("Te0U", "CPU能效核心5", "CPU", 201, &[]),
            sensor("Te0V", "CPU能效核心6", "CPU", 202, &[]),
            sensor("Tg04", "GPU簇1", "GPU", 205, &[]),
            sensor("Tg12", "GPU簇2", "GPU", 206, &[]),
            sensor("Tm1p", "内存邻近", "Other", 208, &[]),
        ],
        aggregates: &[
            AggregateFact {
                id: "avg:cpu",
                name: "CPU核心均值",
                group: "CPU",
                line: 192,
                hide_sensors: false,
                members: &[
                    "Tp01", "Tp05", "Tp09", "Tp0D", "Te05", "Te09", "Te0H", "Te0S", "Te0U", "Te0V",
                ],
            },
            AggregateFact {
                id: "avg:gpu",
                name: "GPU簇均值",
                group: "GPU",
                line: 204,
                hide_sensors: false,
                members: &["Tg04", "Tg12"],
            },
        ],
    },
    Rule {
        scope:
            "MacStudio3,1;Macmini11,2;MacBookPro21,2;MacBookPro21,3;MacBookPro21,4;MacBookPro21,5",
        sensors: &[
            sensor("Tp01", "CPU性能核心1", "CPU", 212, &["CPU性能核心7"]),
            sensor("Tp09", "CPU性能核心2", "CPU", 213, &["CPU性能核心8"]),
            sensor("Tp0H", "CPU性能核心3", "CPU", 214, &["CPU性能核心9"]),
            sensor("Tp0P", "CPU性能核心4", "CPU", 215, &["CPU性能核心10"]),
            sensor("Tp0X", "CPU性能核心5", "CPU", 216, &["CPU性能核心11"]),
            sensor("Tp0Y", "CPU性能核心6", "CPU", 217, &["CPU性能核心12"]),
            sensor("Te05", "CPU能效核心1", "CPU", 218, &["CPU能效核心2"]),
            sensor("Te0S", "CPU能效核心3", "CPU", 219, &["CPU能效核心4"]),
            sensor("Tg05", "GPU簇1", "GPU", 222, &[]),
            sensor("Tg0S", "GPU簇2", "GPU", 223, &[]),
            sensor("Tg0Y", "GPU簇3", "GPU", 224, &[]),
            sensor("Tg0k", "GPU簇4", "GPU", 225, &[]),
            sensor("Tg0z", "GPU簇5", "GPU", 226, &[]),
            sensor("TaLT", "左侧雷雳邻近", "Other", 228, &[]),
            sensor("TaRT", "右侧雷雳邻近", "Other", 229, &[]),
        ],
        aggregates: &[
            AggregateFact {
                id: "avg:cpu",
                name: "CPU核心均值",
                group: "CPU",
                line: 211,
                hide_sensors: false,
                members: &[
                    "Tp01", "Tp09", "Tp0H", "Tp0P", "Tp0X", "Tp0Y", "Te05", "Te0S",
                ],
            },
            AggregateFact {
                id: "avg:gpu",
                name: "GPU簇均值",
                group: "GPU",
                line: 221,
                hide_sensors: false,
                members: &["Tg05", "Tg0S", "Tg0Y", "Tg0k", "Tg0z"],
            },
        ],
    },
    Rule {
        scope: "MacBookPro22,1;MacBookAir14,1;MacBookAir14,2",
        sensors: &[
            sensor("Tp00", "CPU性能核心1", "CPU", 233, &[]),
            sensor("Tp0C", "CPU性能核心2", "CPU", 234, &[]),
            sensor("Tp0O", "CPU性能核心3", "CPU", 235, &[]),
            sensor("Tp0X", "CPU性能核心4", "CPU", 236, &[]),
            sensor("Te04", "CPU能效核心1", "CPU", 237, &["CPU能效核心4"]),
            sensor("Te08", "CPU能效核心2", "CPU", 238, &["CPU能效核心5"]),
            sensor("Te0C", "CPU能效核心3", "CPU", 239, &["CPU能效核心6"]),
            sensor("Tg04", "GPU簇1", "GPU", 242, &[]),
            sensor("Tg12", "GPU簇2", "GPU", 243, &[]),
        ],
        aggregates: &[
            AggregateFact {
                id: "avg:cpu",
                name: "CPU核心均值",
                group: "CPU",
                line: 232,
                hide_sensors: false,
                members: &["Tp00", "Tp0C", "Tp0O", "Tp0X", "Te04", "Te08", "Te0C"],
            },
            AggregateFact {
                id: "avg:gpu",
                name: "GPU簇均值",
                group: "GPU",
                line: 241,
                hide_sensors: false,
                members: &["Tg04", "Tg12"],
            },
        ],
    },
    Rule {
        scope: "MacBookPro22,2+",
        sensors: &[
            sensor("Tm04", "CPU性能核心1", "CPU", 248, &[]),
            sensor("Tm0G", "CPU性能核心2", "CPU", 249, &[]),
            sensor("Tm0R", "CPU性能核心3", "CPU", 250, &[]),
            sensor("Tm0a", "CPU性能核心4", "CPU", 251, &[]),
            sensor("Tm0j", "CPU性能核心5", "CPU", 252, &[]),
            sensor("Tm1Q", "CPU性能核心6", "CPU", 253, &[]),
            sensor("Tm1c", "CPU性能核心7", "CPU", 254, &[]),
            sensor("Tm1o", "CPU性能核心8", "CPU", 255, &[]),
            sensor("Tm21", "CPU性能核心9", "CPU", 256, &[]),
            sensor("Tm2D", "CPU性能核心10", "CPU", 257, &[]),
            sensor("Tm2P", "CPU性能核心11", "CPU", 258, &[]),
            sensor("Tm2n", "CPU性能核心12", "CPU", 259, &[]),
            sensor("Tp04", "CPU超级核心1", "CPU", 260, &[]),
            sensor("Tp0G", "CPU超级核心2", "CPU", 261, &[]),
            sensor("Tp0R", "CPU超级核心3", "CPU", 262, &[]),
            sensor("Tp0a", "CPU超级核心4", "CPU", 263, &[]),
            sensor("Tp0j", "CPU超级核心5", "CPU", 264, &[]),
            sensor("Tp0u", "CPU超级核心6", "CPU", 265, &[]),
            sensor("Tp1I", "CPU超级核心7", "CPU", 266, &[]),
            sensor("Tp1g", "CPU超级核心8", "CPU", 267, &[]),
            sensor("Tg08", "GPU簇1", "GPU", 270, &[]),
            sensor("Tg12", "GPU簇2", "GPU", 271, &[]),
            sensor("Tg1x", "GPU簇3", "GPU", 272, &[]),
            sensor("Tg29", "GPU簇4", "GPU", 273, &[]),
        ],
        aggregates: &[
            AggregateFact {
                id: "avg:cpu",
                name: "CPU核心均值",
                group: "CPU",
                line: 247,
                hide_sensors: false,
                members: &[
                    "Tm04", "Tm0G", "Tm0R", "Tm0a", "Tm0j", "Tm1Q", "Tm1c", "Tm1o", "Tm21", "Tm2D",
                    "Tm2P", "Tm2n", "Tp04", "Tp0G", "Tp0R", "Tp0a", "Tp0j", "Tp0u", "Tp1I", "Tp1g",
                ],
            },
            AggregateFact {
                id: "avg:gpu",
                name: "GPU簇均值",
                group: "GPU",
                line: 269,
                hide_sensors: false,
                members: &["Tg08", "Tg12", "Tg1x", "Tg29"],
            },
        ],
    },
    Rule {
        scope: "MacStudio3,2",
        sensors: &[
            sensor("Tp06", "CPU性能核心1", "CPU", 278, &["CPU性能核心9"]),
            sensor("Tp0E", "CPU性能核心2", "CPU", 279, &["CPU性能核心10"]),
            sensor("Tp0M", "CPU性能核心3", "CPU", 280, &["CPU性能核心11"]),
            sensor("Tp0T", "CPU性能核心4", "CPU", 281, &["CPU性能核心12"]),
            sensor("Tp0W", "CPU性能核心5", "CPU", 282, &["CPU性能核心13"]),
            sensor("Tp0c", "CPU性能核心6", "CPU", 283, &["CPU性能核心14"]),
            sensor("Tp0i", "CPU性能核心7", "CPU", 284, &["CPU性能核心15"]),
            sensor("Tp0o", "CPU性能核心8", "CPU", 285, &["CPU性能核心16"]),
            sensor("Tp18", "CPU性能核心17", "CPU", 286, &["CPU性能核心21"]),
            sensor("Tp1G", "CPU性能核心18", "CPU", 287, &["CPU性能核心22"]),
            sensor("Tp1K", "CPU性能核心19", "CPU", 288, &["CPU性能核心23"]),
            sensor("Tp1S", "CPU性能核心20", "CPU", 289, &["CPU性能核心24"]),
            sensor("Te06", "CPU能效核心1", "CPU", 290, &["CPU能效核心5"]),
            sensor("Te0M", "CPU能效核心2", "CPU", 291, &["CPU能效核心6"]),
            sensor("Te0Q", "CPU能效核心3", "CPU", 292, &["CPU能效核心7"]),
            sensor("Te0T", "CPU能效核心4", "CPU", 293, &["CPU能效核心8"]),
            sensor("Tg05", "GPU簇1", "GPU", 296, &[]),
            sensor("Tg0z", "GPU簇2", "GPU", 297, &[]),
            sensor("Tg22", "GPU簇3", "GPU", 298, &[]),
            sensor("Tg2I", "GPU簇4", "GPU", 299, &[]),
            sensor("Tg34", "GPU簇5", "GPU", 300, &[]),
            sensor("Tg3K", "GPU簇6", "GPU", 301, &[]),
            sensor("TaLT", "左侧雷雳邻近", "Other", 303, &[]),
            sensor("TaRT", "右侧雷雳邻近", "Other", 304, &[]),
        ],
        aggregates: &[
            AggregateFact {
                id: "avg:cpu",
                name: "CPU核心均值",
                group: "CPU",
                line: 277,
                hide_sensors: false,
                members: &[
                    "Tp06", "Tp0E", "Tp0M", "Tp0T", "Tp0W", "Tp0c", "Tp0i", "Tp0o", "Tp18", "Tp1G",
                    "Tp1K", "Tp1S", "Te06", "Te0M", "Te0Q", "Te0T",
                ],
            },
            AggregateFact {
                id: "avg:gpu",
                name: "GPU簇均值",
                group: "GPU",
                line: 295,
                hide_sensors: false,
                members: &["Tg05", "Tg0z", "Tg22", "Tg2I", "Tg34", "Tg3K"],
            },
        ],
    },
    Rule {
        scope: "MacBookNeo1,1",
        sensors: &[
            sensor("TW0p", "无线模块邻近", "Other", 307, &[]),
            sensor("Tp01", "CPU性能核心1", "CPU", 309, &[]),
            sensor("Tp09", "CPU性能核心2", "CPU", 310, &[]),
            sensor("Te05", "CPU能效核心1", "CPU", 311, &[]),
            sensor("Te06", "CPU能效核心2", "CPU", 312, &[]),
            sensor("Tex0", "CPU能效核心3", "CPU", 313, &[]),
            sensor("Tex1", "CPU能效核心4", "CPU", 314, &[]),
            sensor("Tg05", "GPU簇1", "GPU", 317, &[]),
            sensor("Tg0D", "GPU簇2", "GPU", 318, &[]),
        ],
        aggregates: &[
            AggregateFact {
                id: "avg:cpu",
                name: "CPU核心均值",
                group: "CPU",
                line: 308,
                hide_sensors: false,
                members: &["Tp01", "Tp09", "Te05", "Te06", "Tex0", "Tex1"],
            },
            AggregateFact {
                id: "avg:gpu",
                name: "GPU簇均值",
                group: "GPU",
                line: 316,
                hide_sensors: false,
                members: &["Tg05", "Tg0D"],
            },
        ],
    },
];

fn matches_exact(scope: &str, model: &str) -> bool {
    scope
        .split(';')
        .any(|candidate| !candidate.contains('+') && candidate == model)
}

// Deliberately accepts only canonical familyMajor,minor identifiers. The source parser
// is more permissive; malformed or overflowing inputs must not broaden this catalogue.
fn model_version(model: &str) -> Option<(&str, u16)> {
    let start = model.find(|c: char| c.is_ascii_digit())?;
    let (family, version) = model.split_at(start);
    if !matches!(
        family,
        "MacBookPro"
            | "MacBookAir"
            | "MacBookNeo"
            | "MacBook"
            | "iMacPro"
            | "iMac"
            | "Macmini"
            | "MacStudio"
            | "MacPro"
            | "Xserve"
            | "ADP"
    ) {
        return None;
    }
    let (major, minor) = version.split_once(',')?;
    let canonical = |part: &str| {
        !part.is_empty()
            && part.bytes().all(|c| c.is_ascii_digit())
            && (part.len() == 1 || !part.starts_with('0'))
    };
    if !canonical(major) || !canonical(minor) {
        return None;
    }
    let value = major
        .bytes()
        .chain(minor.bytes())
        .try_fold(0u16, |value, digit| {
            value.checked_mul(10)?.checked_add(u16::from(digit - b'0'))
        })?;
    Some((family, value))
}

pub(crate) fn matches_scope(scope: &str, model: &str) -> bool {
    if scope == "all" {
        return true;
    }
    scope.split(';').any(|token| {
        if let Some(lower) = token.strip_suffix('+') {
            match (model_version(lower), model_version(model)) {
                (Some((family, lower)), Some((current_family, current))) => {
                    family == current_family && current >= lower
                }
                _ => false,
            }
        } else {
            !token.contains('+') && token == model
        }
    })
}

/// True only when an exact ARM sensor rule names the normalized model.
/// Range and common rules can apply even when this returns false.
pub fn has_exact_catalogue(model: &str) -> bool {
    RULES
        .iter()
        .any(|rule| matches_exact(rule.scope, normalize_model(model)))
}

fn evidence(source: &'static str, rule: &Rule, line: u16) -> Vec<Evidence> {
    vec![Evidence {
        file: source,
        line,
        model_scope: rule.scope,
    }]
}

fn sensor_descriptor(
    source: &'static str,
    rule: &Rule,
    sensor: &SensorFact,
    hide_sensors: bool,
) -> Descriptor {
    Descriptor {
        id: sensor.key.into(),
        display_name: sensor.name.into(),
        aliases: sensor.aliases.iter().map(|name| (*name).into()).collect(),
        hide_sensors,
        group: sensor.group,
        classification: Classification::ProvisionalMapped,
        member_ids: Vec::new(),
        evidence: evidence(source, rule, sensor.line),
        note: (!sensor.aliases.is_empty()).then_some(ALIAS_NOTE),
    }
}

// Resolve all matching blocks in order. Removing before insertion also clears the
// previous grouping; an excluded key cannot survive in an aggregate's member list.
pub(crate) fn resolve_rules_with_source<'a>(
    source: &'static str,
    model: &str,
    rules: impl IntoIterator<Item = &'a Rule>,
    keys_match: fn(&str, &str) -> bool,
) -> Vec<Descriptor> {
    let mut active: Vec<(Descriptor, Option<(&Rule, &AggregateFact)>)> = Vec::new();
    for rule in rules
        .into_iter()
        .filter(|rule| matches_scope(rule.scope, model))
    {
        for fact in rule.sensors {
            active.retain(|(descriptor, _)| !keys_match(&descriptor.id, fact.key));
            if fact.exclude {
                continue;
            }
            let aggregate = rule
                .aggregates
                .iter()
                .find(|group| group.members.contains(&fact.key));
            active.push((
                sensor_descriptor(
                    source,
                    rule,
                    fact,
                    aggregate.is_some_and(|g| g.hide_sensors),
                ),
                aggregate.map(|g| (rule, g)),
            ));
        }
    }
    let mut aggregates: Vec<Descriptor> = Vec::new();
    for (descriptor, membership) in &active {
        let Some((rule, group)) = membership else {
            continue;
        };
        if let Some(existing) = aggregates.iter_mut().find(|d| d.id == group.id) {
            if !existing.member_ids.contains(&descriptor.id) {
                existing.member_ids.push(descriptor.id.clone());
            }
            for provenance in evidence(source, rule, group.line) {
                if !existing.evidence.contains(&provenance) {
                    existing.evidence.push(provenance);
                }
            }
        } else {
            aggregates.push(Descriptor {
                id: group.id.into(),
                display_name: group.name.into(),
                aliases: Vec::new(),
                hide_sensors: group.hide_sensors,
                group: group.group,
                classification: Classification::ProvisionalMapped,
                member_ids: vec![descriptor.id.clone()],
                evidence: evidence(source, rule, group.line),
                note: Some(AGGREGATE_NOTE),
            });
        }
    }
    let mut result: Vec<_> = active
        .into_iter()
        .map(|(descriptor, _)| descriptor)
        .collect();
    result.extend(aggregates);
    result
}

fn resolve_rules<'a>(model: &str, rules: impl IntoIterator<Item = &'a Rule>) -> Vec<Descriptor> {
    resolve_rules_with_source(ARM_SOURCE, model, rules, |declared, actual| {
        declared == actual
    })
}

/// Select explicit architecture metadata. Unknown spellings do not inherit either catalogue.
/// Key declarations are candidates only; callers must retain actual discovered reading IDs.
pub fn catalogue_for_architecture(architecture: &str, model: &str) -> Vec<Descriptor> {
    match architecture {
        "arm64" | "aarch64" => catalogue(model),
        "x86_64" | "x86" | "i386" => crate::intel_catalog::catalogue(model),
        _ => Vec::new(),
    }
}

/// Metadata matching only. Never alter a hardware read's actual key with this result.
/// ARM is exact; Intel permits only the final ASCII character's case to differ.
pub(crate) fn keys_match_for_architecture(
    architecture: &str,
    declared_key: &str,
    actual_key: &str,
) -> bool {
    match architecture {
        "arm64" | "aarch64" => declared_key == actual_key,
        "x86_64" | "x86" | "i386" => crate::intel_catalog::keys_match(declared_key, actual_key),
        _ => false,
    }
}

/// ARM SMC descriptions, with all/range/exact rules applied in document order.
/// Callers must verify real key presence and reading quality. This API does not
/// select an architecture; do not use it as an implemented Intel catalogue.
pub fn catalogue(model: &str) -> Vec<Descriptor> {
    let model = normalize_model(model);
    let mut result = resolve_rules(model, std::iter::once(&COMMON).chain(RULES));
    // Optional low-Tp reference metadata retains its separate exact-model gate.
    if has_exact_catalogue(model) {
        for (key, line) in [("TCDX", 35), ("TCMb", 41)] {
            result.push(Descriptor {
                id: key.into(),
                display_name: format!("CPU参考温度（{key}）"),
                aliases: Vec::new(),
                hide_sensors: false,
                group: "CPU",
                classification: Classification::ProvisionalMapped,
                member_ids: Vec::new(),
                evidence: vec![Evidence {
                    file: REFERENCE_SOURCE,
                    line,
                    model_scope: REFERENCE_SCOPE,
                }],
                note: Some(REFERENCE_NOTE),
            });
        }
    }
    result
}

/// Preserve every unmatched real key as raw metadata, without assigning a component.
#[cfg_attr(not(test), allow(dead_code))] // Standalone catalogue lookup API; monitoring resolves once per snapshot.
pub fn classify(model: &str, key: &str) -> Descriptor {
    catalogue(model)
        .into_iter()
        .find(|descriptor| descriptor.id == key)
        .unwrap_or_else(|| Descriptor {
            id: key.into(),
            display_name: key.into(),
            aliases: Vec::new(),
            hide_sensors: false,
            group: "Other",
            classification: Classification::Raw,
            member_ids: Vec::new(),
            evidence: Vec::new(),
            note: Some("该机型与键没有已核实的描述对应；保留原始键"),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn normalizes_only_the_46_documented_full_identifiers() {
        assert_eq!(ALIASES.len(), 46);
        assert_eq!(normalize_model("Mac16,8"), "MacBookPro21,2");
        let mut unique = HashSet::new();
        for (source, target, line) in ALIASES {
            assert!(unique.insert(*source));
            assert_eq!(normalize_model(source), *target);
            assert_eq!(normalize_model(target), *target);
            assert_eq!(normalization_evidence(source).unwrap().line, *line);
        }
        for unknown in ["Mac16,80", "Mac16,8 ", "mac16,8", "Mac99,99", ""] {
            assert_eq!(normalize_model(unknown), unknown);
            assert!(normalization_evidence(unknown).is_none());
            assert_eq!(
                classify(unknown, "Tp01").classification,
                Classification::Raw
            );
        }
    }

    #[test]
    fn unknown_arm_models_only_inherit_five_explicit_common_keys() {
        for model in ["Mac99,99", "", "MacPro8,1", "Mac16,8 "] {
            let descriptors = catalogue(model);
            assert_eq!(descriptors.len(), 5);
            assert_eq!(classify(model, "TB0T").display_name, "电池");
            for key in ["Tp01", "avg:cpu", "TCDX", "TCMb"] {
                let d = classify(model, key);
                assert_eq!(d.classification, Classification::Raw);
                assert_eq!(d.display_name, key);
                assert!(d.evidence.is_empty());
                assert!(d.aliases.is_empty());
            }
        }
        assert!(matches_scope("all", "Mac99,99"));
        assert!(!matches_scope("all;Macmini9,1", "Mac99,99"));
    }

    #[test]
    fn ranges_include_lower_bound_preserve_family_and_legacy_digit_comparison() {
        for (scope, lower, below, other) in [
            (
                "MacBookPro17,1+",
                "MacBookPro17,1",
                "MacBookPro16,4",
                "MacBookAir17,1",
            ),
            (
                "MacBookAir10,1+",
                "MacBookAir10,1",
                "MacBookAir9,1",
                "MacBookPro10,1",
            ),
            ("iMac21,1+", "iMac21,1", "iMac20,2", "iMacPro21,1"),
            (
                "MacStudio1,1+",
                "MacStudio1,1",
                "MacStudio1,0",
                "Macmini1,1",
            ),
            (
                "MacBookPro22,2+",
                "MacBookPro22,2",
                "MacBookPro22,1",
                "MacBookAir22,2",
            ),
        ] {
            assert!(matches_scope(scope, lower));
            assert!(!matches_scope(scope, below));
            assert!(!matches_scope(scope, other));
        }
        assert!(matches_scope(
            "MacBookAir10,1+;MacBookPro17,1+",
            "MacBookPro21,2"
        ));
        assert!(matches_scope("MacBookPro2,10+", "MacBookPro21,0")); // both 210
        assert!(!matches_scope("MacBookPro2,11+", "MacBookPro21,0")); // 210 < 211
        assert!(matches_scope("MacBookPro22,2+", "MacBookPro99,1"));
        assert!(!matches_scope("MacBookPro22,2+", "Mac99,1"));
    }

    #[test]
    fn malformed_and_overflowing_ranges_fail_closed() {
        for token in [
            "MacBookPro+22,2",
            "MacBookPro22,2++",
            "+MacBookPro22,2",
            "MacBookPro22,2+ ",
            "MacBookPro22,2x+",
            "MacBookPro022,2+",
            "MacBookPro22,02+",
            "MacBookPro22,+",
            "MacBookPro22,2,1+",
            "MacBookPro6553,6+",
            "Unknown22,2+",
            "macbookpro22,2+",
        ] {
            assert!(!matches_scope(token, "MacBookPro99,1"), "{token}");
        }
        for model in [
            "MacBookPro21,2 ",
            "MacBookPro21,2+",
            "MacBookPro021,2",
            "MacBookPro6553,6",
            "MacBookPro21,2,3",
            "Mac16,8",
        ] {
            assert!(!matches_scope("MacBookPro17,1+", model), "{model}");
        }
        assert_eq!(
            model_version("MacBookPro6553,5"),
            Some(("MacBookPro", 65535))
        );
        assert!(!matches_scope(" MacBookPro17,1+", "MacBookPro21,2"));
    }

    #[test]
    fn model_isolation_and_new_range_descriptions() {
        assert_eq!(catalogue("Mac16,8"), catalogue("MacBookPro21,2"));
        for key in ["Ts0P", "Ts1P", "Tg0S"] {
            assert_eq!(
                classify("Mac16,8", key).classification,
                Classification::ProvisionalMapped
            );
        }
        assert_eq!(
            classify("MacBookPro18,1", "Tg0S").classification,
            Classification::Raw
        );
        assert_eq!(
            classify("Mac16,8", "Tg0D").classification,
            Classification::Raw
        );
        assert_eq!(
            classify("Macmini10,1", "Tp0n").classification,
            Classification::ProvisionalMapped
        );
        assert_eq!(
            classify("Macmini10,2", "Tp0n").classification,
            Classification::Raw
        );
        assert_eq!(classify("MacStudio1,1", "TaFL").display_name, "前左气流");
        assert_eq!(classify("iMac21,1", "TL0p").display_name, "显示屏邻近1");
        assert!(!has_exact_catalogue("Mac17,9"));
        assert_eq!(classify("Mac17,9", "avg:cpu").member_ids.len(), 20);
        assert_eq!(classify("MacBookPro22,3", "avg:gpu").member_ids.len(), 4);
    }

    #[test]
    fn aliases_share_one_physical_source_and_never_duplicate_average_members() {
        let descriptors = catalogue("Mac16,8");
        assert_eq!(descriptors.len(), 26); // common5 + range2 + exact15 + groups2 + references2
        let core = classify("Mac16,8", "Tp01");
        assert_eq!(core.display_name, "CPU性能核心1");
        assert_eq!(core.aliases, ["CPU性能核心7"]);
        assert!(core.note.unwrap().contains("同一读数"));
        assert_eq!(descriptors.iter().filter(|d| d.id == "Tp01").count(), 1);
        assert!(!descriptors.iter().any(|d| d.id == "Tp01-0"));
        assert_eq!(
            classify("Mac16,8", "avg:cpu").member_ids,
            ["Tp01", "Tp09", "Tp0H", "Tp0P", "Tp0X", "Tp0Y", "Te05", "Te0S"]
        );
        assert_eq!(classify("Mac15,14", "avg:cpu").member_ids.len(), 16);
        assert_eq!(classify("Mac15,14", "avg:gpu").member_ids.len(), 6);
        assert!(!classify("Macmini10,1", "avg:cpu")
            .member_ids
            .contains(&"Tp0n".into()));
    }

    #[test]
    fn later_replacement_and_exclusion_clear_previous_membership() {
        const GROUP: AggregateFact = AggregateFact {
            id: "avg:cpu",
            name: "均值",
            group: "CPU",
            line: 1,
            hide_sensors: true,
            members: &["AAAA", "BBBB"],
        };
        const FIRST: Rule = Rule {
            scope: "all",
            sensors: &[
                sensor("AAAA", "早期", "CPU", 1, &["旧别名"]),
                sensor("BBBB", "成员", "CPU", 2, &[]),
            ],
            aggregates: &[GROUP],
        };
        const SECOND: Rule = Rule {
            scope: "MacBookPro21,2",
            sensors: &[
                sensor("AAAA", "后期", "Other", 3, &[]),
                SensorFact {
                    exclude: true,
                    ..sensor("BBBB", "排除", "CPU", 4, &[])
                },
            ],
            aggregates: &[],
        };
        let result = resolve_rules("MacBookPro21,2", [&FIRST, &SECOND]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "AAAA");
        assert_eq!(result[0].display_name, "后期");
        assert_eq!(result[0].group, "Other");
        assert!(result[0].aliases.is_empty());
        assert!(!result[0].hide_sensors);
        assert!(result[0].member_ids.is_empty());
        assert_eq!(result[0].evidence[0].line, 3);
    }

    #[test]
    fn source_group_hide_metadata_does_not_remove_readings() {
        let descriptors = catalogue("MacBookPro20,1");
        assert!(classify("MacBookPro20,1", "Tg0D").hide_sensors);
        assert!(classify("MacBookPro20,1", "avg:gpu").hide_sensors);
        assert_eq!(
            descriptors
                .iter()
                .filter(|d| d.hide_sensors && !d.is_aggregate())
                .count(),
            6
        );
        assert!(!classify("Mac16,8", "Tp01").hide_sensors);
    }

    #[test]
    fn all_catalogues_have_unique_real_keys_and_unique_aggregate_members() {
        for model in RULES
            .iter()
            .flat_map(|r| r.scope.split(';'))
            .map(|m| m.trim_end_matches('+'))
        {
            let descriptors = catalogue(model);
            let mut ids = HashSet::new();
            for d in &descriptors {
                assert!(ids.insert(d.id.as_str()), "duplicate {model}/{}", d.id);
                assert!(matches!(d.group, "CPU" | "GPU" | "Battery" | "Other"));
                assert_eq!(d.classification, Classification::ProvisionalMapped);
                assert!(!d.evidence.is_empty());
                for e in &d.evidence {
                    assert!(e.line > 0);
                    if e.file == ARM_SOURCE {
                        assert!(matches_scope(e.model_scope, model));
                    }
                }
                if d.id.starts_with("avg:") {
                    assert!(d.is_aggregate());
                    let unique: HashSet<_> = d.member_ids.iter().collect();
                    assert_eq!(unique.len(), d.member_ids.len());
                    for member in &d.member_ids {
                        assert_eq!(member.len(), 4);
                        assert_eq!(
                            descriptors
                                .iter()
                                .filter(|source| source.id == *member && !source.is_aggregate())
                                .count(),
                            1
                        );
                    }
                } else {
                    assert_eq!(d.id.len(), 4);
                    assert!(!d.is_aggregate());
                }
            }
        }
    }

    #[test]
    fn counts_cover_all_19_arm_blocks_232_facts_and_24_groups() {
        assert_eq!(RULES.len() + 1, 19);
        let exact: usize = RULES
            .iter()
            .filter(|r| !r.scope.contains('+'))
            .map(|r| r.sensors.len())
            .sum();
        let ranged: usize = RULES
            .iter()
            .filter(|r| r.scope.contains('+'))
            .map(|r| r.sensors.len())
            .sum();
        assert_eq!(exact, 195);
        assert_eq!(COMMON.sensors.len(), 5);
        assert_eq!(ranged, 32);
        assert_eq!(exact + ranged + COMMON.sensors.len(), 195 + 37);
        assert_eq!(
            RULES
                .iter()
                .map(|r| r.scope.split(';').filter(|t| t.ends_with('+')).count())
                .sum::<usize>(),
            5
        );
        assert_eq!(RULES.iter().map(|r| r.aggregates.len()).sum::<usize>(), 24);
        assert!(RULES.iter().flat_map(|r| r.sensors).all(|s| !s.exclude));
    }

    #[test]
    fn unmatched_keys_remain_raw() {
        let d = classify("Mac16,8", "TXYZ");
        assert_eq!(d.display_name, "TXYZ");
        assert_eq!(d.classification.as_str(), "raw");
        assert_eq!(
            Classification::ProvisionalMapped.as_str(),
            "provisional_mapped"
        );
        assert!(d.member_ids.is_empty());
        assert!(d.aliases.is_empty());
        assert!(d.evidence.is_empty());
    }

    #[test]
    fn reference_keys_have_distinct_fallback_evidence_and_never_enter_average() {
        for (key, line) in [("TCDX", 35), ("TCMb", 41)] {
            let d = classify("Mac16,8", key);
            assert_eq!(d.group, "CPU");
            assert!(d.note.unwrap().contains("未证实具体部件或芯片结温"));
            assert!(!d.is_aggregate());
            assert!(d.aliases.is_empty());
            assert_eq!(d.evidence[0].file, REFERENCE_SOURCE);
            assert_eq!(d.evidence[0].line, line);
            assert!(!classify("Mac16,8", "avg:cpu")
                .member_ids
                .iter()
                .any(|m| m == key));
            assert_eq!(
                classify("Mac99,99", key).classification,
                Classification::Raw
            );
            assert_eq!(classify("Mac17,9", key).classification, Classification::Raw);
        }
    }
}
