use crate::events::Activity;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MeshOverview {
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub updated_at: DateTime<Utc>,
    pub nodes: Vec<MeshOverviewNode>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MeshOverviewNode {
    pub id: String,
    pub name: String,
    #[serde(rename = "denUrl")]
    pub url: String,
    pub online: bool,
    pub sessions: Option<u64>,
    pub latest: Option<MeshLatest>,
    pub version: Option<String>,
    pub revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MeshLatest {
    pub activity: Activity,
    pub title: String,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MeshDevice {
    pub id: String,
    pub name: String,
    pub public_key: String,
    pub address: String,
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    #[serde_as(as = "Option<serde_with::TimestampMilliSeconds>")]
    pub enrolled_at: Option<DateTime<Utc>>,
    #[serde(default)]
    #[serde_as(as = "Option<serde_with::TimestampMilliSeconds>")]
    pub last_handshake: Option<DateTime<Utc>>,
    pub pg_role: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeviceListResponse {
    pub devices: Vec<MeshDevice>,
    pub pending: Vec<PendingEnrollment>,
    pub relay_configured: bool,
}

#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PendingEnrollment {
    pub id: String,
    pub name: String,
    pub address: String,
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub created_at: DateTime<Utc>,
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub expires_at: DateTime<Utc>,
}

#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StoredPending {
    pub id: String,
    pub name: String,
    pub token: String,
    pub address: String,
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub created_at: DateTime<Utc>,
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeviceRosterFile {
    pub v: u32,
    pub devices: Vec<MeshDevice>,
    pub pending: Vec<StoredPending>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCreateRequest {
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename = "rivet-mesh-enroll", rename_all = "camelCase")]
pub struct EnrollmentQr {
    pub v: u32,
    pub gateway: String,
    pub token: String,
    pub config: DeviceEnrollConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCreateResponse {
    pub pending: PendingEnrollment,
    pub qr: EnrollmentQr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnrolledDevice {
    pub id: String,
    pub name: String,
    pub address: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SignedCertificate {
    pub leaf: String,
    pub chain: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeviceEnrollResponse {
    pub ok: bool,
    pub device: EnrolledDevice,
    pub config: DeviceEnrollConfig,
    pub certificate: SignedCertificate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PairRedeemResponse {
    pub device_id: String,
    pub certificate: SignedCertificate,
}

#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PairingRecord {
    pub v: u32,
    pub device_id: String,
    pub name: String,
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeviceEnrollConfig {
    pub shared_host: String,
    pub shared_export: String,
    pub pg_url: String,
    pub embed_url: String,
    pub wg_endpoint: String,
    pub wg_peer_public_key: String,
    pub wg_address: String,
    pub wg_allowed_ips: String,
    pub home_subnet: String,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeviceEnrollRequest {
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub public_key: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub csr: Option<String>,
}

#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PhonePairingCode {
    pub device_id: String,
    pub gateway: String,
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub expires_at: DateTime<Utc>,
    pub qr_text: String,
    pub reshown: u32,
    pub added_to_users: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PhonePairingState {
    Pending,
    Paired,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PhonePairingStatus {
    pub state: PhonePairingState,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PhonePairingAvailability {
    pub available: bool,
    pub reason: Option<String>,
    pub gateway: Option<String>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PhonePairingRequest {
    pub name: String,
    #[serde(default)]
    pub host: Option<String>,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Notification {
    #[serde(rename = "escalation")]
    Escalation {
        task_id: String,
        agent_id: String,
        summary: String,
        href: String,
        #[serde_as(as = "serde_with::TimestampMilliSeconds")]
        ts: DateTime<Utc>,
    },
    #[serde(rename = "workflow.gate", rename_all = "camelCase")]
    WorkflowGate {
        run_id: String,
        workflow_id: String,
        label: String,
        prompt: Option<String>,
        href: String,
        #[serde_as(as = "serde_with::TimestampMilliSeconds")]
        ts: DateTime<Utc>,
    },
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VoiceSpeakRequest {
    pub input: String,
    pub instructions: Option<String>,
    pub voice: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VoiceTranscribeResponse {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename = "DeviceFileEntry")]
pub struct FileEntry {
    pub name: String,
    pub r#type: FileEntryKind,
    pub size: u64,
    pub mtime: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum FileEntryKind {
    Dir,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FilesListResponse {
    pub path: String,
    pub root: String,
    pub entries: Vec<FileEntry>,
}

#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StagedUploadResponse {
    pub uri: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub expires_at: DateTime<Utc>,
}
