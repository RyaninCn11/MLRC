//! The install plan: what one version needs on *this* machine.

use crate::error::{ErrorCode, MlrcError, MlrcResult};
use crate::meta::rule::{allowed, Env};
use crate::meta::version::{Artifact, AssetIndex, VersionJson};

#[derive(Debug, Clone)]
pub struct JavaRequirement {
    pub component: String,
    pub major: u32,
}

#[derive(Debug, Clone, Default)]
pub struct InstallPlan {
    pub id: String,
    pub main_class: String,
    pub java: Option<JavaRequirement>,
    pub client: Option<Artifact>,
    pub libraries: Vec<Artifact>,
    pub natives: Vec<Artifact>,
    pub asset_index: Option<AssetIndex>,
    pub logging: Option<Artifact>,
    /// Bytes of the files listed here. Asset objects are excluded: they are only
    /// known after the index is fetched.
    pub bytes: u64,
    pub files: usize,
}

pub fn build(version: &VersionJson, env: &Env) -> MlrcResult<InstallPlan> {
    let java = match &version.java_version {
        Some(java) => JavaRequirement {
            component: java.component.clone(),
            major: java.major_version.unwrap_or(0),
        },
        None => {
            return Err(MlrcError::new(
                ErrorCode::JavaMetaMissing,
                format!(
                    "version {} does not declare a javaVersion block; MLRC only supports versions that do",
                    version.id
                ),
            )
            .with("id", version.id.clone()))
        }
    };

    let mut plan = InstallPlan {
        id: version.id.clone(),
        main_class: version.main_class.clone(),
        java: Some(java),
        client: version.downloads.client.clone(),
        asset_index: version.asset_index.clone(),
        logging: version
            .logging
            .as_ref()
            .and_then(|logging| logging.client.as_ref())
            .map(|client| client.file.clone()),
        ..Default::default()
    };

    for library in &version.libraries {
        if !allowed(&library.rules, env) {
            continue;
        }

        if let Some(natives) = &library.natives {
            if let Some(classifier) = natives.get(env.os.as_str()) {
                if let Some(artifact) = library.downloads.classifier(classifier) {
                    plan.natives.push(artifact.clone());
                }
                continue;
            }
        }

        if let Some(artifact) = &library.downloads.artifact {
            plan.libraries.push(artifact.clone());
        }
    }

    let mut bytes = 0u64;
    let mut files = 0usize;
    for artifact in plan
        .client
        .iter()
        .chain(plan.libraries.iter())
        .chain(plan.natives.iter())
    {
        bytes += artifact.size;
        files += 1;
    }
    if let Some(index) = &plan.asset_index {
        bytes += index.size;
        files += 1;
    }
    if let Some(logging) = &plan.logging {
        bytes += logging.size;
        files += 1;
    }
    plan.bytes = bytes;
    plan.files = files;

    Ok(plan)
}
