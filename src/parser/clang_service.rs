use std::io::{self, BufReader, Read, Write};
#[cfg(not(test))]
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use crossbeam_channel as crossbeam;
use serde::{Deserialize, Serialize};

use crate::files::ecc_frame;
use crate::parser::clang_result::{
    ClangDiagnosticEntry, ClangParseResult, DiagnosticCounts,
};
use crate::parser::semantic_extractor::SemanticExtractor;

static CLANG_PARSE_SERVICE: OnceLock<Result<Arc<ClangParseService>, String>> = OnceLock::new();
static CLANG_PARSE_HELPERS: AtomicUsize = AtomicUsize::new(1);
static SHARED_INDEX_PTR: OnceLock<usize> = OnceLock::new();

const CLANG_PARSE_DEADLINE_SECS: u64 = 30;

#[derive(Debug)]
struct ClangParseRequest {
    source_path: String,
    text: String,
    arguments: Vec<String>,
    response: crossbeam::Sender<Result<ClangParseResult, String>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ClangParseHelperRequest {
    source_path: String,
    text: String,
    arguments: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ClangParseHelperResponse {
    success: bool,
    diagnostics: Vec<String>,
    diagnostic_counts: DiagnosticCounts,
    diagnostic_entries: Vec<ClangDiagnosticEntry>,
    tu_ecc_data: Option<Vec<u8>>,
    tu_source_path: Option<String>,
    error: Option<String>,
}

pub(crate) struct ClangParseService {
    senders: Vec<crossbeam::Sender<ClangParseRequest>>,
    next_helper: AtomicUsize,
}

pub(crate) struct ClangParseHandle {
    response: crossbeam::Receiver<Result<ClangParseResult, String>>,
}

impl ClangParseService {
    pub(crate) fn configure(helper_count: usize) {
        let desired = helper_count.max(1);
        let mut current = CLANG_PARSE_HELPERS.load(Ordering::Relaxed);
        while current < desired {
            match CLANG_PARSE_HELPERS.compare_exchange_weak(
                current,
                desired,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
    }

    pub(crate) fn global() -> Result<Arc<Self>> {
        match CLANG_PARSE_SERVICE.get_or_init(Self::spawn_service) {
            Ok(service) => Ok(service.clone()),
            Err(message) => Err(anyhow!(message.clone())),
        }
    }

    pub(crate) fn run_helper_stdio() -> Result<()> {
        Self::ensure_clang_loaded();
        let cx_index = unsafe { clang_sys::clang_createIndex(0, 0) };
        if cx_index.is_null() {
            return Err(anyhow!("clang_createIndex returned null"));
        }
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut reader = BufReader::new(stdin.lock());
        let mut writer = stdout.lock();
        while let Some(request) = Self::read_payload::<ClangParseHelperRequest, _>(&mut reader)
            .map_err(|err| anyhow!("failed reading clang helper request: {err}"))?
        {
            let response = Self::run_helper_parse(
                cx_index,
                &request.source_path,
                &request.text,
                &request.arguments,
            );
            if Self::write_payload(&mut writer, &response).is_err() {
                return Ok(());
            }
            if writer.flush().is_err() {
                return Ok(());
            }
        }
        Ok(())
    }

    fn run_helper_parse(
        cx_index: clang_sys::CXIndex,
        source_path: &str,
        text: &str,
        arguments: &[String],
    ) -> ClangParseHelperResponse {
        let c_source = std::ffi::CString::new(source_path).unwrap();
        let c_text = std::ffi::CString::new(text).unwrap_or_else(|_| {
            std::ffi::CString::new(text.replace('\0', "")).unwrap()
        });
        let c_args: Vec<std::ffi::CString> = arguments
            .iter()
            .map(|a| std::ffi::CString::new(a.as_str()).unwrap())
            .collect();
        let c_arg_ptrs: Vec<*const std::ffi::c_char> =
            c_args.iter().map(|a| a.as_ptr()).collect();

        let unsaved = clang_sys::CXUnsavedFile {
            Filename: c_source.as_ptr(),
            Contents: c_text.as_ptr(),
            Length: text.len() as std::ffi::c_ulong,
        };

        let mut tu: clang_sys::CXTranslationUnit = std::ptr::null_mut();
        let err = unsafe {
            clang_sys::clang_parseTranslationUnit2(
                cx_index,
                c_source.as_ptr(),
                c_arg_ptrs.as_ptr(),
                c_arg_ptrs.len() as std::ffi::c_int,
                &unsaved as *const _ as *mut _,
                1,
                clang_sys::CXTranslationUnit_DetailedPreprocessingRecord,
                &mut tu,
            )
        };

        if err != clang_sys::CXError_Success || tu.is_null() {
            return ClangParseHelperResponse {
                success: false,
                diagnostics: Vec::new(),
                diagnostic_counts: [0; 5],
                diagnostic_entries: Vec::new(),
                tu_ecc_data: None,
                tu_source_path: None,
                error: Some(format!("libclang parse failed: error code {err}")),
            };
        }

        let (success, diagnostics, diagnostic_summary, diagnostic_entries) =
            SemanticExtractor::extract_diagnostics(tu);

        let tu_ecc_data = (|| -> Result<Vec<u8>> {
            let tu_tmp = tempfile::NamedTempFile::new()?;
            let c_tmp_path = std::ffi::CString::new(
                tu_tmp.path().to_str().unwrap(),
            )
            .unwrap();
            let save_err = unsafe {
                clang_sys::clang_saveTranslationUnit(
                    tu,
                    c_tmp_path.as_ptr(),
                    clang_sys::CXSaveTranslationUnit_None,
                )
            };
            if save_err != 0 {
                return Err(anyhow!("TU save failed: error code {save_err}"));
            }
            let tu_bytes = std::fs::read(tu_tmp.path())?;
            let mut ecc_buf = Vec::new();
            ecc_frame::write_frame(&mut ecc_buf, &tu_bytes)?;
            Ok(ecc_buf)
        })();

        unsafe {
            clang_sys::clang_disposeTranslationUnit(tu);
        }

        match tu_ecc_data {
            Ok(data) => ClangParseHelperResponse {
                success,
                diagnostics,
                diagnostic_counts: diagnostic_summary,
                diagnostic_entries,
                tu_ecc_data: Some(data),
                tu_source_path: Some(source_path.to_string()),
                error: None,
            },
            Err(err) => ClangParseHelperResponse {
                success,
                diagnostics,
                diagnostic_counts: diagnostic_summary,
                diagnostic_entries,
                tu_ecc_data: None,
                tu_source_path: None,
                error: Some(format!("TU binary transfer failed: {err}")),
            },
        }
    }

    fn spawn_service() -> Result<Arc<Self>, String> {
        let helper_count = CLANG_PARSE_HELPERS.load(Ordering::Relaxed).max(1);
        let lane_index = Self::shared_lane_index()?;
        let mut senders = Vec::with_capacity(helper_count);
        for helper_index in 0..helper_count {
            let (sender, receiver) = crossbeam::unbounded::<ClangParseRequest>();
            Self::spawn_lane(helper_index, receiver, lane_index)?;
            senders.push(sender);
        }
        Ok(Arc::new(Self {
            senders,
            next_helper: AtomicUsize::new(0),
        }))
    }

    fn shared_lane_index() -> Result<usize, String> {
        let ptr = SHARED_INDEX_PTR.get_or_init(|| {
            Self::ensure_clang_loaded();
            let cx_index = unsafe { clang_sys::clang_createIndex(0, 0) };
            if cx_index.is_null() {
                panic!("clang_createIndex returned null");
            }
            cx_index as usize
        });
        Ok(*ptr)
    }

    /// Ensure clang-sys runtime library is loaded on the current thread.
    /// With the `runtime` feature, clang-sys may require per-thread loading.
    fn ensure_clang_loaded() {
        if !clang_sys::is_loaded() {
            clang_sys::load().expect("failed loading libclang on lane thread");
        }
    }

    #[cfg(test)]
    pub(crate) fn test_index() -> clang_sys::CXIndex {
        let ptr = Self::shared_lane_index().expect("libclang");
        Self::ensure_clang_loaded();
        ptr as clang_sys::CXIndex
    }

    #[cfg(test)]
    fn spawn_lane(
        helper_index: usize,
        receiver: crossbeam::Receiver<ClangParseRequest>,
        lane_index: usize,
    ) -> Result<(), String> {
        thread::Builder::new()
            .name(format!("clang-parse-lane-{helper_index}"))
            .spawn(move || Self::run_inprocess_lane(receiver, lane_index))
            .map_err(|err| format!("failed spawning clang parse lane: {err}"))?;
        Ok(())
    }

    #[cfg(not(test))]
    fn spawn_lane(
        helper_index: usize,
        receiver: crossbeam::Receiver<ClangParseRequest>,
        lane_index: usize,
    ) -> Result<(), String> {
        let helper = ClangParseHelperProcess::spawn()?;
        thread::Builder::new()
            .name(format!("clang-parse-lane-{helper_index}"))
            .spawn(move || Self::run_subprocess_lane(receiver, helper, lane_index))
            .map_err(|err| format!("failed spawning clang parse lane: {err}"))?;
        Ok(())
    }

    pub(crate) fn parse(
        &self,
        source_path: String,
        text: String,
        arguments: Vec<String>,
    ) -> Result<ClangParseResult> {
        let deadline = Instant::now() + Duration::from_secs(CLANG_PARSE_DEADLINE_SECS);
        self.dispatch(source_path, text, arguments)?
            .collect_deadline(deadline)
    }

    pub(crate) fn dispatch(
        &self,
        source_path: String,
        text: String,
        arguments: Vec<String>,
    ) -> Result<ClangParseHandle> {
        let (response_tx, response_rx) = crossbeam::bounded(1);
        let helper_index = self.next_helper.fetch_add(1, Ordering::Relaxed) % self.senders.len();
        self.senders
            .get(helper_index)
            .ok_or_else(|| anyhow!("clang parse service unavailable"))?
            .send(ClangParseRequest {
                source_path,
                text,
                arguments,
                response: response_tx,
            })
            .map_err(|_| anyhow!("clang parse service unavailable"))?;
        Ok(ClangParseHandle {
            response: response_rx,
        })
    }

    pub(crate) fn parse_batch(
        &self,
        source_path: String,
        text: String,
        argument_sets: Vec<Vec<String>>,
    ) -> Result<Vec<Result<ClangParseResult>>> {
        if argument_sets.is_empty() {
            return Ok(Vec::new());
        }
        let deadline = Instant::now() + Duration::from_secs(CLANG_PARSE_DEADLINE_SECS);
        let mut handles = Vec::with_capacity(argument_sets.len());
        for arguments in argument_sets {
            handles.push(self.dispatch(source_path.clone(), text.clone(), arguments)?);
        }
        let mut results = Vec::with_capacity(handles.len());
        for handle in handles {
            results.push(handle.collect_deadline(deadline));
        }
        Ok(results)
    }

    #[cfg(test)]
    fn run_inprocess_lane(receiver: crossbeam::Receiver<ClangParseRequest>, lane_index: usize) {
        Self::ensure_clang_loaded();
        let cx_index = lane_index as clang_sys::CXIndex;
        while let Ok(request) = receiver.recv() {
            let response = Self::run_helper_parse(
                cx_index,
                &request.source_path,
                &request.text,
                &request.arguments,
            );
            let result = Self::load_tu_from_response(
                cx_index,
                &response,
                &request.source_path,
                &request.text,
            );
            let _ = request.response.send(result);
        }
    }

    #[cfg(not(test))]
    fn run_subprocess_lane(
        receiver: crossbeam::Receiver<ClangParseRequest>,
        mut helper: ClangParseHelperProcess,
        lane_index: usize,
    ) {
        Self::ensure_clang_loaded();
        let cx_index = lane_index as clang_sys::CXIndex;

        while let Ok(request) = receiver.recv() {
            let helper_request = ClangParseHelperRequest {
                source_path: request.source_path.clone(),
                text: request.text.clone(),
                arguments: request.arguments,
            };
            let result = match helper.execute(&helper_request) {
                Ok(response) => {
                    Self::load_tu_from_response(
                        cx_index,
                        &response,
                        &request.source_path,
                        &request.text,
                    )
                }
                Err(err) => {
                    if let Ok(new_helper) = ClangParseHelperProcess::spawn() {
                        helper = new_helper;
                    }
                    Err(err)
                }
            };
            let _ = request.response.send(result);
        }
    }

    fn load_tu_from_response(
        cx_index: clang_sys::CXIndex,
        response: &ClangParseHelperResponse,
        source_path: &str,
        source_text: &str,
    ) -> Result<ClangParseResult, String> {
        if let Some(error) = &response.error {
            if response.tu_ecc_data.is_none() {
                return Err(error.clone());
            }
        }

        let Some(ecc_data) = &response.tu_ecc_data else {
            return Ok(ClangParseResult::with_semantic_offsets(
                response.success,
                response.diagnostics.clone(),
                Vec::new(),
                Default::default(),
                Default::default(),
                response.diagnostic_counts,
                response.diagnostic_entries.clone(),
            ));
        };

        let tu_bytes = ecc_frame::read_frame(&mut ecc_data.as_slice())
            .map_err(|err| format!("ECC decode failed: {err}"))?
            .ok_or_else(|| "ECC frame empty".to_string())?;

        let tu_tmp = tempfile::NamedTempFile::new()
            .map_err(|err| format!("tempfile creation failed: {err}"))?;
        std::fs::write(tu_tmp.path(), &tu_bytes)
            .map_err(|err| format!("tempfile write failed: {err}"))?;

        let created_source = if !source_path.is_empty()
            && !std::path::Path::new(source_path).exists()
        {
            let _ = std::fs::write(source_path, source_text);
            true
        } else {
            false
        };

        let result = (|| -> Result<ClangParseResult, String> {
            let c_path = std::ffi::CString::new(
                tu_tmp.path().to_str().unwrap(),
            )
            .unwrap();
            let tu = unsafe {
                clang_sys::clang_createTranslationUnit(cx_index, c_path.as_ptr())
            };
            if tu.is_null() {
                return Err("failed loading TU from saved AST".to_string());
            }

            let result = SemanticExtractor::extract_symbols_and_offsets(tu, source_path)
                .map(|(symbols, rename_offsets, reference_offsets)| {
                    ClangParseResult::with_semantic_offsets(
                        response.success,
                        response.diagnostics.clone(),
                        symbols,
                        rename_offsets,
                        reference_offsets,
                        response.diagnostic_counts,
                        response.diagnostic_entries.clone(),
                    )
                })
                .map_err(|err| err.to_string());

            unsafe {
                clang_sys::clang_disposeTranslationUnit(tu);
            }
            result
        })();

        if created_source {
            let _ = std::fs::remove_file(source_path);
        }

        result
    }

    fn write_payload<T: Serialize, W: Write>(writer: &mut W, value: &T) -> Result<(), String> {
        let payload = postcard::to_allocvec(value)
            .map_err(|err| format!("failed serializing payload: {err}"))?;
        let len = u64::try_from(payload.len()).map_err(|_| "payload too large".to_string())?;
        writer
            .write_all(len.to_le_bytes().as_slice())
            .map_err(|err| format!("failed writing payload header: {err}"))?;
        writer
            .write_all(payload.as_slice())
            .map_err(|err| format!("failed writing payload body: {err}"))?;
        Ok(())
    }

    fn read_payload<T: for<'de> Deserialize<'de>, R: Read>(
        reader: &mut R,
    ) -> Result<Option<T>, String> {
        let mut header = [0u8; 8];
        let mut read = 0usize;
        while read < header.len() {
            match reader.read(&mut header[read..]) {
                Ok(0) if read == 0 => return Ok(None),
                Ok(0) => return Err("unexpected EOF while reading payload header".to_string()),
                Ok(count) => read += count,
                Err(err) => return Err(format!("failed reading payload header: {err}")),
            }
        }
        let payload_len = usize::try_from(u64::from_le_bytes(header))
            .map_err(|_| "payload length overflow".to_string())?;
        let mut payload = vec![0u8; payload_len];
        reader
            .read_exact(payload.as_mut_slice())
            .map_err(|err| format!("failed reading payload body: {err}"))?;
        let decoded = postcard::from_bytes::<T>(payload.as_slice())
            .map_err(|err| format!("failed decoding payload: {err}"))?;
        Ok(Some(decoded))
    }
}

impl ClangParseHandle {
    pub(crate) fn collect_deadline(self, deadline: Instant) -> Result<ClangParseResult> {
        match self.response.recv_deadline(deadline) {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(message)) => Err(anyhow!(message)),
            Err(crossbeam::RecvTimeoutError::Timeout) => {
                Err(anyhow!("clang parse timed out after {CLANG_PARSE_DEADLINE_SECS}s"))
            }
            Err(crossbeam::RecvTimeoutError::Disconnected) => {
                Err(anyhow!("clang parse service disconnected"))
            }
        }
    }
}

#[cfg(not(test))]
struct ClangParseHelperProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

#[cfg(not(test))]
impl ClangParseHelperProcess {
    fn spawn() -> Result<Self, String> {
        let current_exe = std::env::current_exe()
            .map_err(|err| format!("failed resolving current binary: {err}"))?;
        let mut child = Command::new(current_exe)
            .arg("--clang-parse-helper")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|err| format!("failed spawning clang parse helper: {err}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "clang parse helper stdin unavailable".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "clang parse helper stdout unavailable".to_string())?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }

    const HELPER_TIMEOUT: Duration = Duration::from_secs(20);

    fn execute(
        &mut self,
        request: &ClangParseHelperRequest,
    ) -> Result<ClangParseHelperResponse, String> {
        ClangParseService::write_payload(&mut self.stdin, request)?;
        self.stdin
            .flush()
            .map_err(|err| format!("failed flushing helper stdin: {err}"))?;

        let timeout = Self::HELPER_TIMEOUT;
        let stdout = &mut self.stdout;
        let child = &mut self.child;
        let (tx, rx) = crossbeam::bounded(1);

        thread::scope(|s| {
            s.spawn(|| {
                let result =
                    ClangParseService::read_payload::<ClangParseHelperResponse, _>(stdout);
                let _ = tx.send(result);
            });

            match rx.recv_timeout(timeout) {
                Ok(Ok(Some(response))) => Ok(response),
                Ok(Ok(None)) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    Err("clang parse helper disconnected".to_string())
                }
                Ok(Err(err)) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    Err(format!("clang parse helper failed: {err}"))
                }
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    Err("clang parse helper timed out".to_string())
                }
            }
        })
    }
}

#[cfg(not(test))]
impl Drop for ClangParseHelperProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_parse_to_load_round_trip() {
        let cx_index = ClangParseService::test_index();

        let source = r#"
namespace test_ns {
    int helper_var = 10;
    void helper_fn(int x) {}
}
"#;
        let source_path = "/tmp/test_helper_round_trip.cpp";
        std::fs::write(source_path, source).unwrap();

        let args = vec!["-std=c++17".to_string(), "-x".to_string(), "c++".to_string()];

        let response = ClangParseService::run_helper_parse(cx_index, source_path, source, &args);
        assert!(response.error.is_none(), "helper parse error: {:?}", response.error);
        assert!(response.tu_ecc_data.is_some(), "missing TU ECC data");
        assert!(response.success, "parse should succeed");

        let result = ClangParseService::load_tu_from_response(
            cx_index,
            &response,
            source_path,
            source,
        )
        .expect("load from response");

        assert!(result.success);
        assert!(!result.symbols.is_empty(), "should have symbols");

        let has_helper_var = result
            .symbols
            .iter()
            .any(|s| s.name == "helper_var");
        let has_helper_fn = result
            .symbols
            .iter()
            .any(|s| s.name == "helper_fn");
        assert!(has_helper_var, "should find helper_var");
        assert!(has_helper_fn, "should find helper_fn");

        assert_eq!(
            crate::parser::clang_result::diagnostic_total(&result.diagnostic_counts()),
            result.diagnostics.len(),
            "diagnostic count mismatch"
        );

        let _ = std::fs::remove_file(source_path);
    }
}
