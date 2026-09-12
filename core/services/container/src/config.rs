// Licensed to the Apache Software Foundation (ASF) under one
// or more contributor license agreements.  See the NOTICE file
// distributed with this work for additional information
// regarding copyright ownership.  The ASF licenses this file
// to you under the Apache License, Version 2.0 (the
// "License"); you may not use this file except in compliance
// with the License.  You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing,
// software distributed under the License is distributed on an
// "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.  See the License for the
// specific language governing permissions and limitations
// under the License.

use std::fmt::Debug;
use std::str::FromStr;

use opendal_core::Configurator;
use opendal_core::Error;
use opendal_core::ErrorKind;
use opendal_core::OperatorUri;
use opendal_core::Result;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;

use super::backend::ContainerBuilder;

/// Platform selector for OCI images.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImagePlatform {
    os: String,
    architecture: String,
    variant: Option<String>,
}

impl ImagePlatform {
    /// Create a platform selector from OS and architecture.
    pub fn new(os: impl Into<String>, architecture: impl Into<String>) -> Self {
        Self {
            os: os.into(),
            architecture: architecture.into(),
            variant: None,
        }
    }

    /// Create a `linux/amd64` platform selector.
    pub fn linux_amd64() -> Self {
        Self::new("linux", "amd64")
    }

    /// Create a `linux/arm64` platform selector.
    pub fn linux_arm64() -> Self {
        Self::new("linux", "arm64")
    }

    /// Set platform variant, for example `v7` for `linux/arm/v7`.
    pub fn with_variant(mut self, variant: impl Into<String>) -> Self {
        self.variant = Some(variant.into());
        self
    }

    pub(crate) fn matches(&self, platform: &OciPlatform) -> bool {
        self.os == platform.os
            && self.architecture == platform.architecture
            && normalized_variant(&self.architecture, self.variant.as_deref())
                == normalized_variant(&platform.architecture, platform.variant.as_deref())
    }
}

fn normalized_variant<'a>(architecture: &str, variant: Option<&'a str>) -> &'a str {
    match (architecture, variant.unwrap_or_default()) {
        ("amd64", "v1") | ("arm64", "8" | "v8" | "v8.0") => "",
        ("arm", "" | "7" | "v7") => "v7",
        (_, variant) => variant,
    }
}

impl FromStr for ImagePlatform {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let mut parts = s.split('/');
        let os = parts.next().unwrap_or_default();
        let architecture = parts.next().unwrap_or_default();
        let variant = parts.next();

        if os.is_empty() || architecture.is_empty() || parts.next().is_some() {
            return Err(Error::new(
                ErrorKind::ConfigInvalid,
                "platform must use os/architecture or os/architecture/variant",
            ));
        }

        let mut platform = Self::new(os, architecture);
        if let Some(variant) = variant {
            platform = platform.with_variant(variant);
        }
        Ok(platform)
    }
}

impl std::fmt::Display for ImagePlatform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.variant {
            Some(variant) => write!(f, "{}/{}/{}", self.os, self.architecture, variant),
            None => write!(f, "{}/{}", self.os, self.architecture),
        }
    }
}

impl Serialize for ImagePlatform {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ImagePlatform {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct OciPlatform {
    pub(crate) architecture: String,
    pub(crate) os: String,
    #[serde(default)]
    pub(crate) variant: Option<String>,
}

/// Config for container image service support.
#[derive(Default, Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(default)]
#[non_exhaustive]
pub struct ContainerConfig {
    /// Local OCI image layout directory.
    pub layout: Option<String>,
    /// Image reference name in `index.json`.
    pub reference: Option<String>,
    /// Platform selector for multi-platform images.
    pub platform: Option<ImagePlatform>,
    /// Root path inside the merged image rootfs.
    pub root: Option<String>,
}

impl Configurator for ContainerConfig {
    type Builder = ContainerBuilder;

    fn from_uri(_uri: &OperatorUri) -> Result<Self> {
        Err(Error::new(
            ErrorKind::Unsupported,
            "container service doesn't support uri yet",
        ))
    }

    fn into_builder(self) -> Self::Builder {
        ContainerBuilder { config: self }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_matches_default_variants() {
        assert!(ImagePlatform::linux_arm64().matches(&OciPlatform {
            os: "linux".to_string(),
            architecture: "arm64".to_string(),
            variant: Some("v8".to_string()),
        }));
        assert!(ImagePlatform::linux_amd64().matches(&OciPlatform {
            os: "linux".to_string(),
            architecture: "amd64".to_string(),
            variant: Some("v1".to_string()),
        }));
        assert!(ImagePlatform::new("linux", "arm").matches(&OciPlatform {
            os: "linux".to_string(),
            architecture: "arm".to_string(),
            variant: Some("v7".to_string()),
        }));

        assert!(
            !ImagePlatform::new("linux", "arm")
                .with_variant("v6")
                .matches(&OciPlatform {
                    os: "linux".to_string(),
                    architecture: "arm".to_string(),
                    variant: Some("v7".to_string()),
                })
        );
    }
}
