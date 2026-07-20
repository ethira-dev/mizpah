//! Binary log converters for file ingest (EVTX, pcap, NetFlow/IPFIX).

use crate::file_ingest::IngestError;
use flate2::read::GzDecoder;
use serde_json::{json, Value};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Kind of binary/convertible capture recognized for preprocess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvertKind {
    Evtx,
    Pcap,
    Netflow,
}

impl ConvertKind {
    pub fn format_hint(self) -> &'static str {
        match self {
            ConvertKind::Evtx => "windows_evtx_log",
            ConvertKind::Pcap => "pcap_log",
            ConvertKind::Netflow => "netflow_log",
        }
    }
}

/// Result of converting a binary file into NDJSON lines.
#[derive(Debug)]
pub struct ConvertedLines {
    pub lines: Vec<String>,
    pub format_hint: &'static str,
}

fn file_name_lower(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Strip one trailing `.gz` / `.bz2` for type detection.
fn logical_name(path: &Path) -> String {
    let mut name = file_name_lower(path);
    if let Some(stripped) = name.strip_suffix(".gz") {
        name = stripped.to_string();
    } else if let Some(stripped) = name.strip_suffix(".bz2") {
        name = stripped.to_string();
    }
    name
}

/// True when path should be converted rather than line-read (including compressed forms).
pub fn is_convertible_path(path: &Path) -> bool {
    detect_kind(path).is_some()
}

pub fn detect_kind(path: &Path) -> Option<ConvertKind> {
    let name = logical_name(path);
    if name.ends_with(".evtx") {
        return Some(ConvertKind::Evtx);
    }
    if name.ends_with(".pcap") || name.ends_with(".pcapng") || name.ends_with(".cap") {
        return Some(ConvertKind::Pcap);
    }
    if name.starts_with("nfcapd.") || name.ends_with(".nfcapd") || name.contains("nfcapd") {
        return Some(ConvertKind::Netflow);
    }
    // Magic sniff (uncompressed only — compressed handled via extension above).
    if file_name_lower(path).ends_with(".gz") || file_name_lower(path).ends_with(".bz2") {
        return None;
    }
    sniff_magic(path)
}

fn sniff_magic(path: &Path) -> Option<ConvertKind> {
    let mut f = File::open(path).ok()?;
    let mut buf = [0u8; 8];
    f.read_exact(&mut buf).ok()?;
    // EVTX: "ElfFile\0"
    if &buf[..7] == b"ElfFile" {
        return Some(ConvertKind::Evtx);
    }
    // PCAP classic magic (host or swapped endian)
    let magic = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if magic == 0xa1b2_c3d4 || magic == 0xd4c3_b2a1 || magic == 0xa1b2_3c4d || magic == 0x4d3c_b2a1
    {
        return Some(ConvertKind::Pcap);
    }
    // PCAPNG Section Header Block type 0x0a0d0d0a
    if magic == 0x0a0d_0d0a {
        return Some(ConvertKind::Pcap);
    }
    None
}

/// Materialize path to an on-disk file suitable for external tools (decompress if needed).
fn materialize_for_tools(path: &Path) -> Result<(PathBuf, Option<tempfile::TempPath>), IngestError> {
    let name = file_name_lower(path);
    if name.ends_with(".gz") {
        let file = File::open(path)?;
        let mut decoder = GzDecoder::new(file);
        let mut tmp = tempfile::NamedTempFile::new()?;
        std::io::copy(&mut decoder, &mut tmp)?;
        tmp.flush()?;
        let kept = tmp.into_temp_path();
        let p = kept.to_path_buf();
        Ok((p, Some(kept)))
    } else if name.ends_with(".bz2") {
        let file = File::open(path)?;
        let mut decoder = bzip2::read::BzDecoder::new(file);
        let mut tmp = tempfile::NamedTempFile::new()?;
        std::io::copy(&mut decoder, &mut tmp)?;
        tmp.flush()?;
        let kept = tmp.into_temp_path();
        let p = kept.to_path_buf();
        Ok((p, Some(kept)))
    } else {
        Ok((path.to_path_buf(), None))
    }
}

fn which(cmd: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(cmd);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Convert a recognized binary log file into NDJSON lines + format hint.
pub fn convert_file(path: &Path) -> Result<ConvertedLines, IngestError> {
    let kind = detect_kind(path).ok_or_else(|| {
        IngestError::Message(format!(
            "not a convertible binary log: {}",
            path.display()
        ))
    })?;
    match kind {
        ConvertKind::Evtx => convert_evtx(path),
        ConvertKind::Pcap => convert_pcap(path),
        ConvertKind::Netflow => convert_netflow(path),
    }
}

fn convert_evtx(path: &Path) -> Result<ConvertedLines, IngestError> {
    let (materialized, _keep) = materialize_for_tools(path)?;
    let mut parser = evtx::EvtxParser::from_path(&materialized).map_err(|e| {
        IngestError::Message(format!("evtx open {}: {e}", materialized.display()))
    })?;
    let mut lines = Vec::new();
    for record in parser.records_json_value() {
        let record = record.map_err(|e| IngestError::Message(format!("evtx record: {e}")))?;
        let mut obj = match record.data {
            Value::Object(m) => m,
            other => {
                let mut m = serde_json::Map::new();
                m.insert("Event".into(), other);
                m
            }
        };
        obj.insert(
            "event_record_id".into(),
            json!(record.event_record_id),
        );
        // Flatten common System fields for pack match-keys / display.
        if let Some(system) = obj
            .get("Event")
            .and_then(|e| e.get("System"))
            .cloned()
        {
            if let Some(ts) = system
                .pointer("/TimeCreated/#attributes/SystemTime")
                .or_else(|| system.pointer("/TimeCreated/SystemTime"))
                .cloned()
            {
                obj.insert("@timestamp".into(), ts);
            }
            if let Some(id) = system
                .pointer("/EventID")
                .or_else(|| system.pointer("/EventID/#text"))
                .cloned()
            {
                obj.insert("event_id".into(), id);
            }
            if let Some(ch) = system.get("Channel").cloned() {
                obj.insert("channel".into(), ch);
            }
            if let Some(provider) = system
                .pointer("/Provider/#attributes/Name")
                .or_else(|| system.pointer("/Provider/Name"))
                .cloned()
            {
                obj.insert("provider".into(), provider);
            }
        }
        lines.push(Value::Object(obj).to_string());
    }
    Ok(ConvertedLines {
        lines,
        format_hint: ConvertKind::Evtx.format_hint(),
    })
}

fn convert_pcap(path: &Path) -> Result<ConvertedLines, IngestError> {
    let tshark = which("tshark").ok_or_else(|| {
        IngestError::Message(
            "pcap ingest requires `tshark` on PATH (install Wireshark/tshark)".into(),
        )
    })?;
    let (materialized, _keep) = materialize_for_tools(path)?;
    let output = Command::new(tshark)
        .arg("-r")
        .arg(&materialized)
        .arg("-T")
        .arg("ek")
        .output()
        .map_err(|e| IngestError::Message(format!("tshark failed: {e}")))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(IngestError::Message(format!(
            "tshark failed for {}: {err}",
            path.display()
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // EK format alternates index metadata and source docs; keep JSON objects with layers/timestamp.
        if let Ok(Value::Object(mut obj)) = serde_json::from_str::<Value>(trimmed) {
            if obj.contains_key("layers") || obj.contains_key("timestamp") {
                obj.insert("pcap_source".into(), json!(true));
                lines.push(Value::Object(obj).to_string());
            }
        }
    }
    Ok(ConvertedLines {
        lines,
        format_hint: ConvertKind::Pcap.format_hint(),
    })
}

fn convert_netflow(path: &Path) -> Result<ConvertedLines, IngestError> {
    let nfdump = which("nfdump").ok_or_else(|| {
        IngestError::Message(
            "NetFlow/IPFIX ingest requires `nfdump` on PATH (install nfdump)".into(),
        )
    })?;
    let (materialized, _keep) = materialize_for_tools(path)?;
    let output = Command::new(nfdump)
        .arg("-r")
        .arg(&materialized)
        .arg("-o")
        .arg("json")
        .output()
        .map_err(|e| IngestError::Message(format!("nfdump failed: {e}")))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(IngestError::Message(format!(
            "nfdump failed for {}: {err}",
            path.display()
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = Vec::new();
    // nfdump -o json may emit a JSON array or NDJSON.
    let trimmed = stdout.trim();
    if trimmed.starts_with('[') {
        if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(trimmed) {
            for item in arr {
                if let Value::Object(mut obj) = item {
                    obj.insert("netflow_source".into(), json!(true));
                    lines.push(Value::Object(obj).to_string());
                }
            }
        }
    } else {
        for line in trimmed.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(Value::Object(mut obj)) = serde_json::from_str::<Value>(line) {
                obj.insert("netflow_source".into(), json!(true));
                lines.push(Value::Object(obj).to_string());
            }
        }
    }
    Ok(ConvertedLines {
        lines,
        format_hint: ConvertKind::Netflow.format_hint(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_evtx_extension() {
        assert_eq!(
            detect_kind(Path::new("/tmp/Security.evtx")),
            Some(ConvertKind::Evtx)
        );
        assert_eq!(
            detect_kind(Path::new("/tmp/Security.evtx.gz")),
            Some(ConvertKind::Evtx)
        );
    }

    #[test]
    fn detect_pcap_extension() {
        assert_eq!(
            detect_kind(Path::new("capture.pcapng")),
            Some(ConvertKind::Pcap)
        );
    }

    #[test]
    fn detect_netflow_name() {
        assert_eq!(
            detect_kind(Path::new("nfcapd.202001010000")),
            Some(ConvertKind::Netflow)
        );
    }

    #[test]
    fn convert_sample_evtx() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/sample.evtx");
        assert!(path.exists(), "missing fixture {}", path.display());
        let out = convert_evtx(&path).expect("convert evtx");
        assert!(!out.lines.is_empty());
        assert_eq!(out.format_hint, "windows_evtx_log");
        let v: Value = serde_json::from_str(&out.lines[0]).unwrap();
        assert!(v.get("Event").is_some() || v.get("event_id").is_some());
    }

    #[test]
    fn missing_tshark_errors_clearly() {
        // Only run when tshark is absent.
        if which("tshark").is_some() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.pcap");
        // Minimal pcap magic so detect_kind works via extension.
        std::fs::write(&path, [0xa1, 0xb2, 0xc3, 0xd4, 0, 0, 0, 0]).unwrap();
        let err = convert_pcap(&path).unwrap_err().to_string();
        assert!(err.contains("tshark"), "{err}");
    }
}
