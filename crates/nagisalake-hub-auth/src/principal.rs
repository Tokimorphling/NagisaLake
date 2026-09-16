//! Organization authorization policy: roles, permissions, and principals.

use crate::error::AuthError;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fmt, str::FromStr};

/// Organization role ordered from least to most privileged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Viewer,
    Member,
    Operator,
    Admin,
    Owner,
}

impl Role {
    pub const fn rank(self) -> u8 {
        match self {
            Self::Viewer => 0,
            Self::Member => 1,
            Self::Operator => 2,
            Self::Admin => 3,
            Self::Owner => 4,
        }
    }

    pub const fn allows(self, permission: Permission) -> bool {
        match permission {
            Permission::WorkflowsRead
            | Permission::JobsReadOrganization
            | Permission::QuotaRead => self.rank() >= Self::Viewer.rank(),
            Permission::JobsWrite
            | Permission::JobsCancelOwn
            | Permission::ArtifactsRead
            | Permission::ArtifactsWrite
            | Permission::ApiKeysManageOwn
            | Permission::DevicesRead
            | Permission::DevicesUse
            | Permission::DevicesRegisterOwn
            | Permission::DevicesShareOwn => self.rank() >= Self::Member.rank(),
            Permission::JobsCancelAny
            | Permission::WorkersManage
            | Permission::WorkflowsPublish => self.rank() >= Self::Operator.rank(),
            Permission::MembersManage
            | Permission::ApiKeysManage
            | Permission::QuotaManage
            | Permission::AuditRead => self.rank() >= Self::Admin.rank(),
            Permission::OrganizationDelete => matches!(self, Self::Owner),
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Viewer => "viewer",
            Self::Member => "member",
            Self::Operator => "operator",
            Self::Admin => "admin",
            Self::Owner => "owner",
        })
    }
}

impl FromStr for Role {
    type Err = AuthError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "viewer" => Ok(Self::Viewer),
            "member" => Ok(Self::Member),
            "operator" => Ok(Self::Operator),
            "admin" => Ok(Self::Admin),
            "owner" => Ok(Self::Owner),
            _ => Err(AuthError::InvalidRole(value.into())),
        }
    }
}

/// Stable service permissions used by browser roles and API-key scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Permission {
    WorkflowsRead,
    WorkflowsPublish,
    JobsReadOrganization,
    JobsWrite,
    JobsCancelOwn,
    JobsCancelAny,
    ArtifactsRead,
    ArtifactsWrite,
    WorkersManage,
    MembersManage,
    ApiKeysManageOwn,
    ApiKeysManage,
    QuotaRead,
    QuotaManage,
    AuditRead,
    OrganizationDelete,
    DevicesRead,
    DevicesUse,
    DevicesRegisterOwn,
    DevicesShareOwn,
}

impl Permission {
    pub const fn scope(self) -> &'static str {
        match self {
            Self::WorkflowsRead => "workflows:read",
            Self::WorkflowsPublish => "workflows:write",
            Self::JobsReadOrganization => "jobs:read",
            Self::JobsWrite => "jobs:write",
            Self::JobsCancelOwn | Self::JobsCancelAny => "jobs:cancel",
            Self::ArtifactsRead => "artifacts:read",
            Self::ArtifactsWrite => "artifacts:write",
            Self::WorkersManage => "workers:manage",
            Self::MembersManage => "members:manage",
            Self::ApiKeysManageOwn | Self::ApiKeysManage => "api_keys:manage",
            Self::QuotaRead => "quota:read",
            Self::QuotaManage => "quota:manage",
            Self::AuditRead => "audit:read",
            Self::OrganizationDelete => "organizations:delete",
            Self::DevicesRead => "devices:read",
            Self::DevicesUse => "devices:use",
            Self::DevicesRegisterOwn => "devices:register",
            Self::DevicesShareOwn => "devices:share",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalKind {
    BrowserSession,
    ApiKey,
    WorkerCredential,
    LegacyToken,
}

/// Authenticated actor passed from transport authentication into services.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub kind:            PrincipalKind,
    pub actor_id:        String,
    pub user_id:         Option<String>,
    pub organization_id: String,
    pub role:            Role,
    pub scopes:          BTreeSet<String>,
}

impl Principal {
    /// Both organization role and API-key scope must authorize the operation.
    pub fn allows(&self, permission: Permission) -> bool {
        if !self.role.allows(permission) {
            return false;
        }
        self.kind != PrincipalKind::ApiKey || self.scopes.contains(permission.scope())
    }
}
