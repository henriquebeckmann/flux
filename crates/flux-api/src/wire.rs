//! The protocol between the launcher (Core) and an Extension Host process.
//!
//! One connection per host, over a socketpair the host inherits (there is no path anyone else
//! could connect to). Frames are a `u32` little-endian length followed by that many bytes of
//! JSON. Every frame carries the protocol version, so either side can refuse or adapt.
//!
//! **Evolution rules** (docs/protocol.md): new message kinds and new optional fields may be added
//! in a minor protocol revision; a receiver ignores unknown fields and answers or skips unknown
//! kinds (`Unknown`). Removing or changing the meaning of anything bumps [`PROTOCOL_VERSION`] and
//! the `Init`/`Ready` handshake picks the highest version both sides speak.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{FieldValue, Limits, PermissionSet, RuntimeEvent};

/// Highest protocol revision this build speaks.
pub const PROTOCOL_VERSION: u32 = 1;
/// Lowest revision it still understands.
pub const MIN_PROTOCOL_VERSION: u32 = 1;
/// Larger frames are a protocol violation (the peer is dropped, nothing is allocated for them).
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame<T> {
    /// Protocol revision of the sender.
    pub v: u32,
    /// Sender-assigned sequence number (diagnostics, ordering checks).
    pub id: u64,
    /// For replies: the `id` of the request answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corr: Option<u64>,
    pub body: T,
}

/// Core → Host.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToHost {
    /// First message: everything the host needs. The host answers [`FromHost::Ready`].
    Init(Init),
    Run { command_id: String, name: String },
    Invoke { view_id: u64, action_id: u64 },
    Submit { view_id: u64, text: String },
    Search { view_id: u64, text: String },
    Selection { view_id: u64, key: Option<String> },
    FormChanged { view_id: u64, field: String, value: FieldValue },
    FormSubmit { view_id: u64, values: Vec<(String, FieldValue)> },
    ViewClosed { view_id: u64 },
    CancelCommand { command_id: String },
    /// Run `on_unload` hooks, stop everything and answer [`FromHost::Unloaded`].
    Unload,
    /// Liveness probe; answered with [`FromHost::Pong`] by a thread that is not the Lua worker.
    Ping,
    /// Answer to a [`FromHost::Call`] (`corr` is the call's `id`).
    Reply { result: Result<serde_json::Value, String> },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Init {
    pub proto_min: u32,
    pub proto_max: u32,
    pub extension: String,
    pub generation: u64,
    /// The extension's folder (read-only for the host).
    pub dir: String,
    /// The extension's private data folder (the only place it may write).
    pub data_dir: String,
    pub app_version: String,
    pub limits: Limits,
    /// What the extension may do. The host enforces it as a first layer; the broker in the
    /// launcher enforces its own copy, which the host cannot influence.
    pub policy: PermissionSet,
    /// First view id this host may hand out (ranges never overlap between hosts).
    pub view_id_base: u64,
    pub sandbox: SandboxMode,
    pub max_open_files: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode {
    /// Apply every restriction; refuse to run the extension if one cannot be applied.
    Strict,
    /// Apply what the kernel offers and report what is missing.
    BestEffort,
    /// No OS-level confinement (tests, development on old kernels).
    Off,
}

/// Host → Core.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FromHost {
    /// The extension is loaded. `sandbox` says what was *actually* applied.
    Ready { proto: u32, sandbox: SandboxReport },
    /// The host could not start (bad manifest, sandbox refused…). It exits right after.
    Failed { message: String },
    Event { event: RuntimeEvent },
    /// A request that needs the launcher (storage, secrets, privileged operations).
    Call { call: HostCall },
    /// Abort a pending call (the extension stopped waiting for it).
    CancelCall { call: u64 },
    Pong { busy_ms: Option<u64>, stats: Stats },
    Unloaded,
    #[serde(other)]
    Unknown,
}

/// What confinement the host applied to itself. Anything not `true` is *not* in effect.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxReport {
    pub mode: Option<SandboxMode>,
    pub no_new_privs: bool,
    pub rlimits: bool,
    /// Landlock ABI version used for filesystem restriction (`None`: not applied).
    pub landlock_abi: Option<u32>,
    /// Landlock restricts TCP connect (ABI ≥ 4).
    pub landlock_net: bool,
    pub seccomp: bool,
    /// Required restrictions that are not in effect, in words.
    pub missing: Vec<String>,
    /// Restrictions that are nice to have and absent on this kernel (they do not fail strict mode).
    #[serde(default)]
    pub notes: Vec<String>,
}

impl SandboxReport {
    /// True only if every restriction the design relies on is in effect.
    pub fn fully_confined(&self) -> bool {
        self.no_new_privs && self.rlimits && self.landlock_abi.is_some() && self.seccomp && self.missing.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stats {
    pub tasks: u32,
    pub open_views: u32,
    pub http_in_flight: u32,
    pub children: u32,
    pub lua_memory_kb: u64,
}

/// A request from the host. The extension's identity is the connection, never a field here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum HostCall {
    LocalGet { key: String },
    LocalSet { key: String, json: String },
    LocalRemove { key: String },
    LocalClear,
    CacheGet { key: String },
    CacheSet { key: String, json: String, ttl_secs: u64 },
    PrefGet { name: String },
    PrefSet { name: String, json: String },
    SecretGet { key: String },
    SecretSet { key: String, value: String },
    SecretRemove { key: String },
    /// A privileged `flux.<namespace>.<function>` call, performed by the launcher after it
    /// checks the permission again. `args` are the Lua arguments as JSON.
    Privileged { namespace: String, function: String, args: Vec<serde_json::Value> },
    #[serde(other)]
    Unknown,
}

// ------------------------------------------------------------------ framing

/// Writes one frame.
pub fn write_frame<W: Write, T: Serialize>(w: &mut W, id: u64, corr: Option<u64>, body: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(&Frame { v: PROTOCOL_VERSION, id, corr, body }).map_err(io::Error::other)?;
    if bytes.len() > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    // One write: concurrent writers behind a mutex never interleave partial frames.
    let mut buf = Vec::with_capacity(bytes.len() + 4);
    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(&bytes);
    w.write_all(&buf)?;
    w.flush()
}

/// Reads one frame. `Ok(None)` is a clean end of stream between frames; a frame that is too
/// large, truncated or not JSON is an `InvalidData` error (the caller drops the peer).
pub fn read_frame<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<Option<Frame<T>>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("frame of {len} bytes exceeds the {MAX_FRAME} byte limit")));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).map_err(|e| if e.kind() == io::ErrorKind::UnexpectedEof { io::Error::new(io::ErrorKind::InvalidData, "truncated frame") } else { e })?;
    serde_json::from_slice(&buf).map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("malformed frame: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T: Serialize + DeserializeOwned>(body: &T) -> Frame<serde_json::Value> {
        let mut buf = Vec::new();
        write_frame(&mut buf, 7, Some(3), body).unwrap();
        read_frame(&mut buf.as_slice()).unwrap().unwrap()
    }

    #[test]
    fn frames_round_trip_with_header_fields() {
        let f = roundtrip(&ToHost::Run { command_id: "a:b".into(), name: "b".into() });
        assert_eq!((f.v, f.id, f.corr), (PROTOCOL_VERSION, 7, Some(3)));
        assert_eq!(f.body["kind"], "run");
    }

    #[test]
    fn unknown_kinds_decode_as_unknown_and_unknown_fields_are_ignored() {
        let f: Frame<ToHost> = serde_json::from_str(r#"{"v":9,"id":1,"body":{"kind":"from_the_future","x":1}}"#).unwrap();
        assert!(matches!(f.body, ToHost::Unknown));
        let f: Frame<ToHost> = serde_json::from_str(r#"{"v":1,"id":1,"body":{"kind":"ping","extra":true}}"#).unwrap();
        assert!(matches!(f.body, ToHost::Ping));
        let f: Frame<FromHost> = serde_json::from_str(r#"{"v":2,"id":1,"body":{"kind":"telemetry"}}"#).unwrap();
        assert!(matches!(f.body, FromHost::Unknown));
        let f: Frame<FromHost> = serde_json::from_str(r#"{"v":1,"id":1,"body":{"kind":"call","call":{"op":"brand_new"}}}"#).unwrap();
        assert!(matches!(f.body, FromHost::Call { call: HostCall::Unknown }));
    }

    #[test]
    fn oversized_truncated_and_malformed_frames_are_errors_not_panics() {
        let mut huge = ((MAX_FRAME as u32) + 1).to_le_bytes().to_vec();
        huge.extend_from_slice(b"{}");
        let e = read_frame::<_, ToHost>(&mut huge.as_slice()).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);

        let mut trunc = 100u32.to_le_bytes().to_vec();
        trunc.extend_from_slice(b"{\"v\":1");
        assert_eq!(read_frame::<_, ToHost>(&mut trunc.as_slice()).unwrap_err().kind(), io::ErrorKind::InvalidData);

        let mut bad = 5u32.to_le_bytes().to_vec();
        bad.extend_from_slice(b"nope!");
        assert_eq!(read_frame::<_, ToHost>(&mut bad.as_slice()).unwrap_err().kind(), io::ErrorKind::InvalidData);

        assert!(read_frame::<_, ToHost>(&mut [].as_slice()).unwrap().is_none(), "clean EOF between frames");
    }

    #[test]
    fn events_survive_the_wire() {
        let ev = RuntimeEvent::ExtensionCrashed { extension: "x".into(), generation: 4, reason: "boom".into(), views: vec![1, 2] };
        let mut buf = Vec::new();
        write_frame(&mut buf, 1, None, &FromHost::Event { event: ev }).unwrap();
        let f: Frame<FromHost> = read_frame(&mut buf.as_slice()).unwrap().unwrap();
        let FromHost::Event { event: RuntimeEvent::ExtensionCrashed { views, generation, .. } } = f.body else { panic!() };
        assert_eq!((views, generation), (vec![1, 2], 4));
    }

    #[test]
    fn sandbox_report_only_counts_what_is_in_effect() {
        let mut r = SandboxReport { no_new_privs: true, rlimits: true, landlock_abi: Some(4), seccomp: true, ..Default::default() };
        assert!(r.fully_confined());
        r.missing.push("landlock net".into());
        assert!(!r.fully_confined());
    }
}

#[cfg(test)]
mod ready_tests {
    use super::*;

    #[test]
    fn a_ready_frame_with_missing_optional_fields_parses() {
        let json = r#"{"v":1,"id":1,"body":{"kind":"ready","proto":1,"sandbox":{"mode":"strict","no_new_privs":true,"rlimits":true,"landlock_abi":null,"landlock_net":false,"seccomp":false,"missing":[]}}}"#;
        let f: Frame<FromHost> = serde_json::from_str(json).unwrap();
        let FromHost::Ready { sandbox, .. } = f.body else { panic!() };
        assert!(!sandbox.fully_confined());
    }
}
