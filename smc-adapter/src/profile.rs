use crate::{AdapterError, Identity, Key, Metadata};
use macs_fan_control_core::{FanId, TargetEncoding};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    SharedMask,
    PerFan { lowercase: bool, ftst: bool },
}

pub(crate) struct FanSpec {
    pub fan: FanId,
    pub target_metadata: Metadata,
    pub min_metadata: Metadata,
    pub max_metadata: Metadata,
    pub mode_metadata: Metadata,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub allow_zero: bool,
}

/// This record is compiled and reviewed with the adapter, never deserialized.
/// Each case must refer to an independent hardware recovery evidence artifact.
pub(crate) struct RecoveryEvidence {
    pub artifact: &'static str,
    pub sha256: &'static str,
    pub automatic_restore: bool,
    pub staged_target: bool,
    pub disconnect_watchdog: bool,
    pub helper_crash: bool,
    pub sleep_wake: bool,
    pub competing_controller: bool,
}

pub(crate) struct Profile {
    pub model: &'static str,
    pub architecture: &'static str,
    pub route: Route,
    pub count_metadata: Metadata,
    pub mask_metadata: Option<Metadata>,
    pub gate_metadata: Option<Metadata>,
    pub fans: &'static [FanSpec],
    pub evidence: RecoveryEvidence,
}

// Protocol implementation does not establish hardware restoration safety.
// No available artifact proves these required conditions for a production model.
// Future entries require code review and the evidence listed in README.md.
static PRODUCTION_PROFILES: &[Profile] = &[];

pub(crate) fn select(identity: &Identity) -> Option<&'static Profile> {
    PRODUCTION_PROFILES.iter().find(|profile| {
        profile.model == identity.model && profile.architecture == identity.architecture
    })
}

pub(crate) fn refusal(identity: &Identity) -> String {
    if identity.model == "Mac16,8" {
        "Mac16,8 → MacBookPro21,2 is inside the analyzed vendor not-working predicate (202≤version<216); no independent automatic-restoration validation exists. Hardware writes are refused.".into()
    } else {
        format!("No reviewed hardware restoration profile for {}/{}; readable keys and implemented protocol do not establish verified control support.", identity.model, identity.architecture)
    }
}

impl Profile {
    pub fn validate(&self, identity: &Identity) -> Result<(), AdapterError> {
        let evidence = &self.evidence;
        let good_record = !evidence.artifact.is_empty()
            && evidence.sha256.len() == 64
            && evidence.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            && evidence.automatic_restore
            && evidence.staged_target
            && evidence.disconnect_watchdog
            && evidence.helper_crash
            && evidence.sleep_wake
            && evidence.competing_controller;
        if self.model != identity.model || self.architecture != identity.architecture {
            return Err(AdapterError::IdentityChanged);
        }
        if !good_record || self.fans.is_empty() || self.fans.len() > 16 {
            return Err(AdapterError::Denied {
                reason: "Hardware recovery evidence is incomplete".into(),
            });
        }
        let mode_metadata_valid = match self.route {
            Route::SharedMask => self
                .mask_metadata
                .is_some_and(|metadata| metadata.same_encoding(Metadata::UI16)),
            Route::PerFan { ftst, .. } => {
                !ftst
                    || self
                        .gate_metadata
                        .is_some_and(|metadata| metadata.same_encoding(Metadata::UI8))
            }
        };
        if !self.count_metadata.same_encoding(Metadata::UI8) || !mode_metadata_valid {
            return Err(AdapterError::Denied {
                reason: "Profile mode/count metadata is invalid".into(),
            });
        }
        for (index, spec) in self.fans.iter().enumerate() {
            if spec.fan.0 as usize != index
                || !spec.min.is_finite()
                || !spec.max.is_finite()
                || !spec.step.is_finite()
                || spec.min < 0.0
                || spec.max < spec.min
                || spec.max > 30000.0
                || spec.step <= 0.0
                || spec.step > spec.max
                || (spec.min == 0.0 && !spec.allow_zero)
                || spec.target_metadata.target_encoding() == TargetEncoding::Unknown
                || !spec.min_metadata.same_encoding(spec.target_metadata)
                || !spec.max_metadata.same_encoding(spec.target_metadata)
                || !spec.mode_metadata.same_encoding(Metadata::UI8)
            {
                return Err(AdapterError::Denied {
                    reason: "Hardware profile parameters are invalid".into(),
                });
            }
            crate::encode_target(spec.target_metadata, spec.min)?;
            crate::encode_target(spec.target_metadata, spec.max)?;
            crate::encode_target(spec.target_metadata, spec.step)?;
        }
        Ok(())
    }

    pub fn fan(&self, fan: FanId) -> Result<&FanSpec, AdapterError> {
        self.fans
            .get(fan.0 as usize)
            .filter(|spec| spec.fan == fan)
            .ok_or(AdapterError::UnknownFan { fan: fan.0 })
    }

    fn permits(&self, key: Key, metadata: Metadata) -> bool {
        match key {
            Key::Target(fan) => self
                .fan(fan)
                .is_ok_and(|spec| spec.target_metadata == metadata),
            Key::Mask => self.route == Route::SharedMask && Some(metadata) == self.mask_metadata,
            Key::ModeUpper(fan) => {
                matches!(
                    self.route,
                    Route::PerFan {
                        lowercase: false,
                        ..
                    }
                ) && self
                    .fan(fan)
                    .is_ok_and(|spec| metadata == spec.mode_metadata)
            }
            Key::ModeLower(fan) => {
                matches!(
                    self.route,
                    Route::PerFan {
                        lowercase: true,
                        ..
                    }
                ) && self
                    .fan(fan)
                    .is_ok_and(|spec| metadata == spec.mode_metadata)
            }
            Key::Gate => {
                matches!(self.route, Route::PerFan { ftst: true, .. })
                    && Some(metadata) == self.gate_metadata
            }
            _ => false,
        }
    }
}

/// Private, operation-bound capability; no constructor is exposed to callers.
pub(crate) struct Admission {
    profile: &'static Profile,
    key: Key,
    metadata: Metadata,
}

impl Admission {
    pub fn for_write(
        profile: &'static Profile,
        identity: &Identity,
        key: Key,
        metadata: Metadata,
    ) -> Result<Self, AdapterError> {
        profile.validate(identity)?;
        if !profile.permits(key, metadata) {
            return Err(AdapterError::Denied {
                reason: "Key or metadata is outside the trusted profile".into(),
            });
        }
        Ok(Self {
            profile,
            key,
            metadata,
        })
    }

    pub fn verify(
        &self,
        identity: &Identity,
        key: Key,
        metadata: Metadata,
    ) -> Result<(), AdapterError> {
        self.profile.validate(identity)?;
        if key != self.key || metadata != self.metadata || !self.profile.permits(key, metadata) {
            return Err(AdapterError::Denied {
                reason: "Write admission does not cover this operation".into(),
            });
        }
        Ok(())
    }

    /// Defense in depth: even a test fixture token cannot authorize a real device.
    pub fn verify_real_registry(&self) -> Result<(), AdapterError> {
        if !PRODUCTION_PROFILES
            .iter()
            .any(|profile| std::ptr::eq(profile, self.profile))
        {
            return Err(AdapterError::Denied {
                reason: "Admission is not from the compiled production hardware registry".into(),
            });
        }
        Ok(())
    }
}
