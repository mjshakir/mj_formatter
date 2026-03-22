use std::io::{self, BufReader, Read, Write};
#[cfg(not(test))]
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use clang::{Clang, Index};
use crossbeam_channel as crossbeam;
use serde::{Deserialize, Serialize};

use crate::parser::clang_result::ClangParseResult;
use crate::parser::semantic_extractor::SemanticExtractor;

static CLANG_PARSE_SERVICE: OnceLock<Result<Arc<ClangParseService>, String>> = OnceLock::new();
static CLANG_PARSE_HELPERS: AtomicUsize = AtomicUsize::new(1);

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
    result: Result<ClangParseResult, String>,
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
        let clang = Clang::new().map_err(|err| anyhow!("failed loading libclang: {err}"))?;
        let index = Index::new(&clang, false, false);
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut reader = BufReader::new(stdin.lock());
        let mut writer = stdout.lock();
        while let Some(request) = Self::read_payload::<ClangParseHelperRequest, _>(&mut reader)
            .map_err(|err| anyhow!("failed reading clang helper request: {err}"))?
        {
            let response = ClangParseHelperResponse {
                result: Self::run_parse(
                    &index,
                    request.source_path.as_str(),
                    request.text.as_str(),
                    request.arguments.as_slice(),
                ),
            };
            if Self::write_payload(&mut writer, &response).is_err() {
                return Ok(()); // Broken pipe at shutdown — parent dropped the pipe.
            }
            if writer.flush().is_err() {
                return Ok(());
            }
        }
        Ok(())
    }

    fn spawn_service() -> Result<Arc<Self>, String> {
        let helper_count = CLANG_PARSE_HELPERS.load(Ordering::Relaxed).max(1);
        let mut senders = Vec::with_capacity(helper_count);
        for helper_index in 0..helper_count {
            let (sender, receiver) = crossbeam::unbounded::<ClangParseRequest>();
            Self::spawn_lane(helper_index, receiver)?;
            senders.push(sender);
        }
        Ok(Arc::new(Self {
            senders,
            next_helper: AtomicUsize::new(0),
        }))
    }

    #[cfg(test)]
    fn spawn_lane(
        helper_index: usize,
        receiver: crossbeam::Receiver<ClangParseRequest>,
    ) -> Result<(), String> {
        thread::Builder::new()
            .name(format!("clang-parse-lane-{helper_index}"))
            .spawn(move || Self::run_inprocess_lane(receiver))
            .map_err(|err| format!("failed spawning clang parse lane: {err}"))?;
        Ok(())
    }

    #[cfg(not(test))]
    fn spawn_lane(
        helper_index: usize,
        receiver: crossbeam::Receiver<ClangParseRequest>,
    ) -> Result<(), String> {
        let helper = ClangParseHelperProcess::spawn()?;
        thread::Builder::new()
            .name(format!("clang-parse-lane-{helper_index}"))
            .spawn(move || Self::run_subprocess_lane(receiver, helper))
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
    fn run_inprocess_lane(receiver: crossbeam::Receiver<ClangParseRequest>) {
        let clang = match Clang::new() {
            Ok(value) => value,
            Err(err) => {
                let message = format!("failed loading libclang: {err}");
                while let Ok(request) = receiver.recv() {
                    let _ = request.response.send(Err(message.clone()));
                }
                return;
            }
        };
        let index = Index::new(&clang, false, false);
        while let Ok(request) = receiver.recv() {
            let result = Self::run_parse(
                &index,
                request.source_path.as_str(),
                request.text.as_str(),
                request.arguments.as_slice(),
            );
            let _ = request.response.send(result);
        }
    }

    #[cfg(not(test))]
    fn run_subprocess_lane(
        receiver: crossbeam::Receiver<ClangParseRequest>,
        mut helper: ClangParseHelperProcess,
    ) {
        while let Ok(request) = receiver.recv() {
            let helper_request = ClangParseHelperRequest {
                source_path: request.source_path,
                text: request.text,
                arguments: request.arguments,
            };
            let result = helper.execute(&helper_request);
            if result.is_err() {
                if let Ok(new_helper) = ClangParseHelperProcess::spawn() {
                    helper = new_helper;
                }
            }
            let _ = request.response.send(result);
        }
    }

    fn run_parse(
        index: &Index<'_>,
        source_path: &str,
        text: &str,
        arguments: &[String],
    ) -> Result<ClangParseResult, String> {
        SemanticExtractor::run_parse(index, source_path, text, arguments)
            .map_err(|err| err.to_string())
    }

    fn write_payload<T: Serialize, W: Write>(writer: &mut W, value: &T) -> Result<(), String> {
        let payload = bincode::serde::encode_to_vec(value, bincode::config::standard())
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
        let (decoded, consumed) = bincode::serde::decode_from_slice::<T, _>(
            payload.as_slice(),
            bincode::config::standard(),
        )
        .map_err(|err| format!("failed decoding payload: {err}"))?;
        if consumed != payload.len() {
            return Err(format!(
                "payload decode consumed {consumed} of {} bytes",
                payload.len()
            ));
        }
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

    fn execute(&mut self, request: &ClangParseHelperRequest) -> Result<ClangParseResult, String> {
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
                Ok(Ok(Some(response))) => response.result,
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
