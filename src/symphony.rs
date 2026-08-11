use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::events::AppEvent;

const POLL_INTERVAL: Duration = Duration::from_secs(15);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const LIST_QUERY: &str = "ExecutionStatus = \"Running\"";
const FLOW_TYPE: &str = "symphonyFlow";
const QUESTION_TYPE: &str = "questionLoopWorkflow";
const FLOW_QUERY: &str = "symphony-start-attestation-v1";
const QUESTION_QUERY: &str = "symphony-question-state-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Workflow {
    pub(crate) workflow_id: String,
    pub(crate) run_id: String,
    pub(crate) name: String,
    pub(crate) phase: String,
    pub(crate) wait: Option<String>,
    pub(crate) started_at: Option<String>,
    pub(crate) ticket: Option<String>,
    pub(crate) repo: Option<String>,
    pub(crate) pr: Option<String>,
    pub(crate) receipts: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Snapshot {
    pub(crate) workflows: Vec<Workflow>,
    pub(crate) unavailable: Option<String>,
}

impl Snapshot {
    fn available(workflows: Vec<Workflow>) -> Self {
        Self {
            workflows,
            unavailable: None,
        }
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            workflows: Vec::new(),
            unavailable: Some(message.into()),
        }
    }
}

trait TemporalCommand {
    fn run_json(&self, args: &[String]) -> Result<serde_json::Value, String>;
}

struct TemporalCli {
    address: String,
    namespace: String,
}

impl Default for TemporalCli {
    fn default() -> Self {
        Self {
            address: std::env::var("TEMPORAL_ADDRESS")
                .unwrap_or_else(|_| "127.0.0.1:7233".to_string()),
            namespace: std::env::var("TEMPORAL_NAMESPACE")
                .unwrap_or_else(|_| "default".to_string()),
        }
    }
}

impl TemporalCli {
    fn scoped(&self, mut args: Vec<String>) -> Vec<String> {
        args.extend([
            "--address".to_string(),
            self.address.clone(),
            "--namespace".to_string(),
            self.namespace.clone(),
            "--output".to_string(),
            "json".to_string(),
        ]);
        args
    }
}

impl TemporalCommand for TemporalCli {
    fn run_json(&self, args: &[String]) -> Result<serde_json::Value, String> {
        let mut child = Command::new("temporal")
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("could not start Temporal CLI: {error}"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Temporal CLI stdout was not captured".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "Temporal CLI stderr was not captured".to_string())?;
        let stdout_reader = std::thread::spawn(move || drain_output(stdout));
        let stderr_reader = std::thread::spawn(move || {
            let mut reader = stderr;
            let _ = std::io::copy(&mut reader, &mut std::io::sink());
        });
        let deadline = Instant::now() + COMMAND_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let stdout = stdout_reader
                        .join()
                        .map_err(|_| "Temporal CLI output reader failed".to_string())?
                        .map_err(|error| format!("could not read Temporal CLI output: {error}"))?;
                    let _ = stderr_reader.join();
                    if !status.success() {
                        return Err("Temporal runtime is unreachable".to_string());
                    }
                    return serde_json::from_slice(&stdout)
                        .map_err(|error| format!("invalid Temporal CLI JSON: {error}"));
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    return Err("Temporal CLI timed out".to_string());
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    return Err(format!("could not wait for Temporal CLI: {error}"));
                }
            }
        }
    }
}

fn drain_output(mut reader: impl Read) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(output);
        }
        let remaining = MAX_OUTPUT_BYTES.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..read.min(remaining)]);
    }
}

#[derive(Debug, Clone)]
struct Execution {
    workflow_id: String,
    run_id: String,
    workflow_type: String,
    started_at: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct FlowContext {
    ticket: Option<String>,
    run_digest: Option<String>,
    title: Option<String>,
    repo: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct QuestionContext {
    ticket: Option<String>,
    run_digest: Option<String>,
    wait: Option<String>,
}

pub(crate) fn poll() -> Snapshot {
    poll_with(&TemporalCli::default())
}

fn poll_with(cli: &impl TemporalCommand) -> Snapshot {
    let production = TemporalCli::default();
    let list_args = production.scoped(vec![
        "workflow".to_string(),
        "list".to_string(),
        "--query".to_string(),
        LIST_QUERY.to_string(),
        "--limit".to_string(),
        "1000".to_string(),
    ]);
    let list = match cli.run_json(&list_args) {
        Ok(value) => value,
        Err(error) => return Snapshot::unavailable(error),
    };
    let executions = match parse_executions(&list) {
        Ok(value) => value,
        Err(error) => return Snapshot::unavailable(error),
    };

    let mut questions = Vec::new();
    for execution in executions
        .iter()
        .filter(|execution| execution.workflow_type == QUESTION_TYPE)
    {
        if let Ok(value) = cli.run_json(&query_args(&production, execution, QUESTION_QUERY)) {
            if let Some(context) = parse_question_context(&value) {
                questions.push(context);
            }
        }
    }

    let mut workflows = Vec::new();
    for execution in executions
        .iter()
        .filter(|execution| execution.workflow_type == FLOW_TYPE)
    {
        let context = cli
            .run_json(&query_args(&production, execution, FLOW_QUERY))
            .ok()
            .and_then(|value| parse_flow_context(&value))
            .unwrap_or_default();
        let phase = cli
            .run_json(&describe_args(&production, execution))
            .ok()
            .and_then(|value| pending_activity_name(&value))
            .unwrap_or_else(|| "running".to_string());
        let wait = questions
            .iter()
            .find(|question| question_matches_flow(question, &context))
            .and_then(|question| question.wait.clone());
        let receipts = context.run_digest.as_deref().and_then(receipts_dir);
        let pr = receipts.as_deref().and_then(latest_pr_context);
        workflows.push(Workflow {
            workflow_id: execution.workflow_id.clone(),
            run_id: execution.run_id.clone(),
            name: context
                .title
                .clone()
                .or_else(|| context.ticket.clone())
                .unwrap_or_else(|| execution.workflow_id.clone()),
            phase,
            wait,
            started_at: execution.started_at.clone(),
            ticket: context.ticket,
            repo: context.repo,
            pr,
            receipts,
        });
    }
    workflows.sort_by(|left, right| left.started_at.cmp(&right.started_at));
    Snapshot::available(workflows)
}

fn query_args(cli: &TemporalCli, execution: &Execution, query: &str) -> Vec<String> {
    cli.scoped(vec![
        "workflow".to_string(),
        "query".to_string(),
        "--workflow-id".to_string(),
        execution.workflow_id.clone(),
        "--run-id".to_string(),
        execution.run_id.clone(),
        "--name".to_string(),
        query.to_string(),
    ])
}

fn describe_args(cli: &TemporalCli, execution: &Execution) -> Vec<String> {
    cli.scoped(vec![
        "workflow".to_string(),
        "describe".to_string(),
        "--workflow-id".to_string(),
        execution.workflow_id.clone(),
        "--run-id".to_string(),
        execution.run_id.clone(),
    ])
}

fn parse_executions(value: &serde_json::Value) -> Result<Vec<Execution>, String> {
    let rows = value
        .as_array()
        .or_else(|| {
            value
                .get("executions")
                .and_then(serde_json::Value::as_array)
        })
        .or_else(|| {
            value
                .get("workflowExecutions")
                .and_then(serde_json::Value::as_array)
        })
        .ok_or_else(|| "Temporal list JSON has no workflow executions".to_string())?;
    Ok(rows.iter().filter_map(parse_execution).collect())
}

fn parse_execution(value: &serde_json::Value) -> Option<Execution> {
    let execution = value.get("execution").unwrap_or(value);
    let workflow_id =
        string_at(execution, &["workflowId"]).or_else(|| string_at(value, &["workflowId"]))?;
    let run_id = string_at(execution, &["runId"]).or_else(|| string_at(value, &["runId"]))?;
    let workflow_type = string_at(value, &["type", "name"])
        .or_else(|| string_at(value, &["workflowType", "name"]))
        .or_else(|| string_at(value, &["workflowType"]))?;
    let started_at = string_at(value, &["startTime"])
        .or_else(|| string_at(value, &["executionTime"]))
        .or_else(|| string_at(value, &["workflowExecutionInfo", "startTime"]));
    Some(Execution {
        workflow_id,
        run_id,
        workflow_type,
        started_at,
    })
}

fn parse_flow_context(value: &serde_json::Value) -> Option<FlowContext> {
    let value = unwrap_query_value(value);
    let steps = value.get("steps").and_then(serde_json::Value::as_array);
    Some(FlowContext {
        ticket: string_at(value, &["ticketId"]),
        run_digest: string_at(value, &["runProvenance", "runDigest"])
            .or_else(|| string_at(value, &["runId"])),
        title: string_at(value, &["title"]),
        repo: steps.and_then(|steps| steps.iter().find_map(|step| string_at(step, &["repo"]))),
    })
}

fn parse_question_context(value: &serde_json::Value) -> Option<QuestionContext> {
    let value = unwrap_query_value(value);
    if string_at(value, &["lifecycle", "state"]).as_deref() != Some("blocked_needs_input") {
        return None;
    }
    let question_id = string_at(value, &["question", "questionId"]);
    let prompt = string_at(value, &["question", "prompt"]);
    Some(QuestionContext {
        ticket: string_at(value, &["ticketId"]),
        run_digest: string_at(value, &["runDigest"]),
        wait: question_id.or(prompt),
    })
}

fn unwrap_query_value(value: &serde_json::Value) -> &serde_json::Value {
    value
        .get("queryResult")
        .or_else(|| value.get("result"))
        .unwrap_or(value)
}

fn question_matches_flow(question: &QuestionContext, flow: &FlowContext) -> bool {
    (question.run_digest.is_some() && question.run_digest == flow.run_digest)
        || (question.ticket.is_some() && question.ticket == flow.ticket)
}

fn pending_activity_name(value: &serde_json::Value) -> Option<String> {
    let activities = value
        .get("pendingActivities")
        .or_else(|| value.pointer("/workflowExecutionInfo/pendingActivities"))?
        .as_array()?;
    activities.iter().find_map(|activity| {
        string_at(activity, &["activityType", "name"])
            .or_else(|| string_at(activity, &["activityType"]))
    })
}

fn string_at(value: &serde_json::Value, path: &[&str]) -> Option<String> {
    path.iter()
        .try_fold(value, |value, key| value.get(*key))?
        .as_str()
        .map(str::to_string)
}

fn receipts_dir(run_digest: &str) -> Option<String> {
    let root = std::env::var_os("SYMPHONY_FLOW_RUN_ROOT")
        .or_else(|| std::env::var_os("SYMPHONY_RUN_ROOT"))
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".local/state/symphony-temporal/runs"))
        })?;
    Some(
        root.join(format!("flow-{run_digest}"))
            .to_string_lossy()
            .into_owned(),
    )
}

fn latest_pr_context(receipts: &str) -> Option<String> {
    let mut paths = std::fs::read_dir(receipts)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("pr.json"))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .into_iter()
        .rev()
        .find_map(|path| read_pr_context(&path))
}

fn read_pr_context(path: &Path) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    string_at(&value, &["url"]).or_else(|| {
        value
            .get("number")
            .and_then(serde_json::Value::as_u64)
            .map(|number| number.to_string())
    })
}

pub(crate) fn start_poller(event_tx: tokio::sync::mpsc::Sender<AppEvent>) {
    if cfg!(test) {
        return;
    }
    std::thread::spawn(move || loop {
        let snapshot = poll();
        match event_tx.try_send(AppEvent::SymphonyWorkflowsRefreshed { snapshot }) {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => return,
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!("dropped Symphony workflow refresh because the event queue is full");
            }
        }
        std::thread::sleep(POLL_INTERVAL);
    });
}

pub(crate) fn repo_name(repo: &str) -> Option<&str> {
    repo.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|value| !value.is_empty())
}

pub(crate) fn common_checkout(repo: &str) -> Option<PathBuf> {
    let name = repo_name(repo)?;
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    [home.join("Repos").join(name), home.join("repos").join(name)]
        .into_iter()
        .find(|path| path.is_dir())
}

pub(crate) fn launch_env(workflow: &Workflow) -> HashMap<String, String> {
    let mut env = HashMap::from([
        (
            "SYMPHONY_WORKFLOW_ID".to_string(),
            workflow.workflow_id.clone(),
        ),
        ("SYMPHONY_RUN_ID".to_string(), workflow.run_id.clone()),
    ]);
    for (key, value) in [
        ("SYMPHONY_TICKET", workflow.ticket.as_ref()),
        ("SYMPHONY_REPO", workflow.repo.as_ref()),
        ("SYMPHONY_PR", workflow.pr.as_ref()),
        ("SYMPHONY_RECEIPTS", workflow.receipts.as_ref()),
    ] {
        if let Some(value) = value {
            env.insert(key.to_string(), value.clone());
        }
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FixtureCli {
        responses: std::sync::Mutex<Vec<Result<serde_json::Value, String>>>,
        calls: std::sync::Mutex<Vec<Vec<String>>>,
    }

    impl FixtureCli {
        fn new(responses: Vec<Result<serde_json::Value, String>>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses.into_iter().rev().collect()),
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().expect("fixture calls").clone()
        }
    }

    impl TemporalCommand for FixtureCli {
        fn run_json(&self, args: &[String]) -> Result<serde_json::Value, String> {
            self.calls
                .lock()
                .expect("fixture calls")
                .push(args.to_vec());
            self.responses
                .lock()
                .expect("fixture responses")
                .pop()
                .expect("fixture response")
        }
    }

    #[test]
    fn parses_open_flow_with_phase_wait_and_context() {
        let cli = FixtureCli::new(vec![
            Ok(serde_json::json!([
                {"execution":{"workflowId":"symphony-MAT-138-a","runId":"flow-run"},"type":{"name":"symphonyFlow"},"startTime":"2026-08-11T08:00:00Z"},
                {"execution":{"workflowId":"question-MAT-138","runId":"question-run"},"type":{"name":"questionLoopWorkflow"}}
            ])),
            Ok(
                serde_json::json!({"ticketId":"MAT-138","runDigest":"digest","lifecycle":{"state":"blocked_needs_input"},"question":{"questionId":"plan-sign-off","prompt":"Ship?"}}),
            ),
            Ok(
                serde_json::json!({"ticketId":"MAT-138","runId":"digest","title":"Temporal blocker dashboard","steps":[{"repo":"matthias-scale/herdr"}]}),
            ),
            Ok(serde_json::json!({"pendingActivities":[{"activityType":{"name":"runFlowStep"}}]})),
        ]);
        let snapshot = poll_with(&cli);
        assert_eq!(snapshot.unavailable, None);
        assert_eq!(snapshot.workflows.len(), 1);
        let workflow = &snapshot.workflows[0];
        assert_eq!(workflow.name, "Temporal blocker dashboard");
        assert_eq!(workflow.phase, "runFlowStep");
        assert_eq!(workflow.wait.as_deref(), Some("plan-sign-off"));
        assert_eq!(workflow.ticket.as_deref(), Some("MAT-138"));
        assert_eq!(workflow.repo.as_deref(), Some("matthias-scale/herdr"));
        assert!(cli.calls().iter().all(|args| {
            matches!(
                args.get(1).map(String::as_str),
                Some("list" | "query" | "describe")
            )
        }));
    }

    #[test]
    fn connected_empty_list_is_distinct_from_runtime_error() {
        let empty = poll_with(&FixtureCli::new(vec![Ok(serde_json::json!([]))]));
        assert!(empty.workflows.is_empty());
        assert_eq!(empty.unavailable, None);

        let unavailable = poll_with(&FixtureCli::new(vec![Err("offline".to_string())]));
        assert!(unavailable.workflows.is_empty());
        assert_eq!(unavailable.unavailable.as_deref(), Some("offline"));
    }

    #[test]
    fn launch_env_is_display_context_only() {
        let workflow = Workflow {
            workflow_id: "wf".to_string(),
            run_id: "run".to_string(),
            name: "name".to_string(),
            phase: "running".to_string(),
            wait: None,
            started_at: None,
            ticket: Some("MAT-138".to_string()),
            repo: Some("matthias-scale/herdr".to_string()),
            pr: Some("https://github.com/matthias-scale/herdr/pull/1".to_string()),
            receipts: Some("/receipts".to_string()),
        };
        let env = launch_env(&workflow);
        assert_eq!(env["SYMPHONY_WORKFLOW_ID"], "wf");
        assert!(!env.keys().any(|key| {
            ["CANCEL", "SIGNAL", "MERGE", "TERMINATE"]
                .iter()
                .any(|authority| key.contains(authority))
        }));
    }
}
