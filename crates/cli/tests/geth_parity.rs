//! Compares `evm2 t8n` with the stored output of go-ethereum on the committed parity cases.
//!
//! Every case under `testdata/geth-parity/cases` runs for each fork and tracer that has stored
//! geth output (see `scripts/geth_parity.py --bless`). A difference fails the test unless
//! `known-diffs.json` lists it, and a listed difference that no longer occurs fails it too, so a
//! fix deletes its entries. The comparison follows `scripts/geth_parity.py`, which documents the
//! normalisation.

use evm2_cli::t8n::{Inputs, TraceKind, Tracing, execute, parse_fork};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

const CONTEXT_FIELDS: [&str; 4] =
    ["blockHash", "blockNumber", "transactionHash", "transactionPosition"];
const OMIT_EMPTY: [&str; 4] = ["memory", "returnData", "refund", "storage"];

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/geth-parity")
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(
        &fs::read(path).unwrap_or_else(|err| panic!("{}: {err}", path.display())),
    )
    .unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

/// A known difference: every field is a glob where `*` matches any text.
struct Atom {
    case: String,
    fork: String,
    tracer: String,
    path: String,
    hits: AtomicUsize,
}

fn glob(pattern: &str, text: &str) -> bool {
    let parts = pattern.split('*').collect::<Vec<_>>();
    let [first, rest @ ..] = parts.as_slice() else { return false };
    let Some(mut remaining) = text.strip_prefix(first) else { return false };
    let Some((last, middle)) = rest.split_last() else { return remaining.is_empty() };
    for part in middle {
        let Some(at) = remaining.find(part) else { return false };
        remaining = &remaining[at + part.len()..];
    }
    remaining.ends_with(last)
}

fn strings(value: Option<&Value>) -> Vec<String> {
    match value {
        None => vec!["*".to_string()],
        Some(Value::String(text)) => vec![text.clone()],
        Some(Value::Array(items)) => {
            items.iter().map(|item| item.as_str().unwrap().to_string()).collect()
        }
        Some(other) => panic!("unexpected known-diff field {other}"),
    }
}

fn known_atoms() -> Vec<Atom> {
    let known = read_json(&data_dir().join("known-diffs.json"));
    let mut atoms = Vec::new();
    for entry in known["known"].as_array().unwrap() {
        for case in strings(entry.get("case")) {
            for fork in strings(entry.get("fork")) {
                for tracer in strings(entry.get("tracer")) {
                    for path in strings(entry.get("path")) {
                        atoms.push(Atom {
                            case: case.clone(),
                            fork: fork.clone(),
                            tracer: tracer.clone(),
                            path,
                            hits: AtomicUsize::new(0),
                        });
                    }
                }
            }
        }
    }
    atoms
}

fn lower_hex(value: &mut Value) {
    match value {
        Value::String(text) => {
            if text.starts_with("0x") && text[2..].bytes().all(|byte| byte.is_ascii_hexdigit()) {
                *text = text.to_ascii_lowercase();
            }
        }
        Value::Array(items) => items.iter_mut().for_each(lower_hex),
        Value::Object(map) => map.values_mut().for_each(lower_hex),
        _ => {}
    }
}

fn is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.is_empty() || text == "0x",
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        Value::Number(number) => number.as_u64() == Some(0),
        _ => false,
    }
}

fn normalize_trace(tracer: &str, mut value: Value) -> Value {
    match tracer {
        "flat" => {
            for frame in value.as_array_mut().into_iter().flatten() {
                if let Some(frame) = frame.as_object_mut() {
                    for field in CONTEXT_FIELDS {
                        frame.remove(field);
                    }
                }
            }
        }
        "structlog" => {
            for log in value["structLogs"].as_array_mut().into_iter().flatten() {
                if let Some(log) = log.as_object_mut() {
                    log.retain(|key, value| {
                        !OMIT_EMPTY.contains(&key.as_str()) || !is_empty(value)
                    });
                }
            }
            if value["structLogs"].is_null() {
                value["structLogs"] = json!([]);
            }
        }
        _ => {}
    }
    lower_hex(&mut value);
    value
}

fn normalize_result(mut result: Value) -> Value {
    if let Some(rejected) = result.get_mut("rejected").and_then(Value::as_array_mut) {
        *rejected = rejected.iter().map(|entry| json!({ "index": entry["index"] })).collect();
    }
    lower_hex(&mut result);
    result
}

enum Segment {
    Key(String),
    Index,
}

/// Collects the field path of every difference, with array indexes as `[]` and addresses and
/// hashes in keys as `<address>` and `<hash>`.
fn diff(a: &Value, b: &Value, path: &mut Vec<Segment>, found: &mut BTreeMap<String, String>) {
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => {
            let keys = a.keys().chain(b.keys()).collect::<BTreeSet<_>>();
            for key in keys {
                path.push(Segment::Key(key.clone()));
                match (a.get(key), b.get(key)) {
                    (Some(a), Some(b)) => diff(a, b, path, found),
                    (a, b) => record(path, a, b, found),
                }
                path.pop();
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            for index in 0..a.len().max(b.len()) {
                path.push(Segment::Index);
                match (a.get(index), b.get(index)) {
                    (Some(a), Some(b)) => diff(a, b, path, found),
                    (a, b) => record(path, a, b, found),
                }
                path.pop();
            }
        }
        (Value::Number(a), Value::Number(b)) if a.as_f64() == b.as_f64() => {}
        (a, b) if a == b => {}
        (a, b) => record(path, Some(a), Some(b), found),
    }
}

fn record(
    path: &[Segment],
    a: Option<&Value>,
    b: Option<&Value>,
    found: &mut BTreeMap<String, String>,
) {
    let mut text = String::new();
    for segment in path {
        match segment {
            Segment::Index => text.push_str("[]"),
            Segment::Key(key) => {
                if !text.is_empty() {
                    text.push('.');
                }
                text.push_str(&match key.strip_prefix("0x").map(str::len) {
                    Some(40) if key[2..].bytes().all(|byte| byte.is_ascii_hexdigit()) => {
                        "<address>".to_string()
                    }
                    Some(64) if key[2..].bytes().all(|byte| byte.is_ascii_hexdigit()) => {
                        "<hash>".to_string()
                    }
                    _ => key.clone(),
                });
            }
        }
    }
    let show = |value: Option<&Value>| {
        value.map_or("<missing>".to_string(), |v| v.to_string().chars().take(90).collect())
    };
    found
        .entry(text.clone())
        .or_insert_with(|| format!("{text}: geth={} evm2={}", show(a), show(b)));
}

fn tracing(tracer: &str, spec: &Value) -> Tracing {
    match spec["kind"].as_str().unwrap() {
        "result" => Tracing::none(),
        "jsonl" => Tracing::struct_logger(TraceKind::Jsonl, true, false, true),
        "structlog" => Tracing::struct_logger(TraceKind::StructLog, true, false, true),
        "tracer" => {
            let name = match spec.get("js") {
                Some(file) => fs::read_to_string(data_dir().join(file.as_str().unwrap())).unwrap(),
                None => spec["tracer"].as_str().unwrap().to_string(),
            };
            Tracing::tracer(&name, Some(&spec["config"].to_string()))
                .unwrap_or_else(|err| panic!("{tracer}: {err}"))
        }
        kind => panic!("unknown tracer kind {kind}"),
    }
}

struct Job {
    case: String,
    fork: String,
    directory: PathBuf,
    /// The tracers the case runs for the fork, as `scripts/geth_parity.py` selects them.
    tracers: Vec<String>,
}

/// Adds the system contracts of the predeploy list that are needed from `fork` on.
fn with_predeploys(alloc: &Value, fork: &str, predeploys: &Value) -> Value {
    let order = predeploys["forkOrder"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap())
        .collect::<Vec<_>>();
    let index = |name: &str| order.iter().position(|candidate| *candidate == name).unwrap();
    let mut alloc = alloc.as_object().unwrap().clone();
    for item in predeploys["predeploys"].as_array().unwrap() {
        if index(fork) >= index(item["minFork"].as_str().unwrap()) {
            for (address, account) in item["alloc"].as_object().unwrap() {
                if !alloc.keys().any(|key| key.eq_ignore_ascii_case(address)) {
                    alloc.insert(address.clone(), account.clone());
                }
            }
        }
    }
    Value::Object(alloc)
}

/// Runs one case and fork against its stored geth output; returns the known-diff-less failures.
fn check(
    job: &Job,
    specs: &Map<String, Value>,
    predeploys: &Value,
    atoms: &[Atom],
    ran: &Mutex<Vec<(String, String, String)>>,
) -> Vec<String> {
    let stored = job.directory.join("expected").join(format!("{}.json", job.fork));
    if !stored.exists() {
        return vec![format!("{} {}: no stored geth output, run `--bless`", job.case, job.fork)];
    }
    let bundle = read_json(&stored);
    let alloc =
        with_predeploys(&read_json(&job.directory.join("alloc.json")), &job.fork, predeploys);
    let inputs = Inputs {
        alloc: serde_json::from_value(alloc).unwrap(),
        env: serde_json::from_value(read_json(&job.directory.join("env.json"))).unwrap(),
        txs: serde_json::from_value(read_json(&job.directory.join("txs.json"))).unwrap(),
        spec: parse_fork(&job.fork).unwrap(),
        reward: 0,
    };
    let mut failures = Vec::new();
    for tracer in job.tracers.iter().cloned() {
        if tracer != "result" && bundle["traces"].get(&tracer).is_none() {
            failures.push(format!("{} {} {tracer}: no stored geth output", job.case, job.fork));
            continue;
        }
        ran.lock().unwrap().push((job.case.clone(), job.fork.clone(), tracer.clone()));
        let output = execute(&inputs, &tracing(&tracer, &specs[&tracer]))
            .unwrap_or_else(|err| panic!("{} {} {tracer}: {err}", job.case, job.fork));
        let mut found = BTreeMap::new();
        if tracer == "result" {
            let result = normalize_result(output.result);
            diff(
                &normalize_result(bundle["result"].clone()),
                &result,
                &mut vec![Segment::Key("result".into())],
                &mut found,
            );
            let mut alloc = output.alloc;
            lower_hex(&mut alloc);
            diff(&bundle["alloc"], &alloc, &mut vec![Segment::Key("alloc".into())], &mut found);
        } else {
            let traces = output
                .traces
                .iter()
                .map(|trace| {
                    let value = match trace.extension {
                        "jsonl" => Value::Array(
                            trace
                                .content
                                .lines()
                                .map(|line| serde_json::from_str(line).unwrap())
                                .collect(),
                        ),
                        _ => serde_json::from_str(&trace.content).unwrap(),
                    };
                    normalize_trace(&tracer, value)
                })
                .collect::<Vec<_>>();
            let expected = bundle["traces"][&tracer].as_array().unwrap();
            if expected.len() != traces.len() {
                found.insert(
                    "<traces>".to_string(),
                    format!("<traces>: geth={} evm2={}", expected.len(), traces.len()),
                );
            }
            for (geth, evm2) in expected.iter().zip(&traces) {
                diff(&normalize_trace(&tracer, geth.clone()), evm2, &mut Vec::new(), &mut found);
            }
        }
        for (path, example) in &found {
            let mut known = false;
            for atom in atoms {
                if glob(&atom.case, &job.case)
                    && glob(&atom.fork, &job.fork)
                    && glob(&atom.tracer, &tracer)
                    && glob(&atom.path, path)
                {
                    atom.hits.fetch_add(1, Ordering::Relaxed);
                    known = true;
                }
            }
            if !known {
                failures.push(format!("{} {} {tracer} {example}", job.case, job.fork));
            }
        }
    }
    failures
}

#[test]
fn matches_geth_except_known_diffs() {
    let dir = data_dir();
    let specs = read_json(&dir.join("tracers.json")).as_object().unwrap().clone();
    let predeploys = read_json(&dir.join("predeploys.json"));
    let atoms = known_atoms();

    let order = predeploys["forkOrder"]
        .as_array()
        .unwrap()
        .iter()
        .map(|fork| fork.as_str().unwrap())
        .collect::<Vec<_>>();
    let position = |fork: &str| order.iter().position(|candidate| *candidate == fork).unwrap();
    let mut jobs = Vec::new();
    let mut cases = fs::read_dir(dir.join("cases"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    cases.sort();
    for directory in cases {
        let meta = read_json(&directory.join("case.json"));
        for fork in meta["forks"].as_array().unwrap() {
            let fork = fork.as_str().unwrap();
            // Only a node runs the tracers other than `result` and `jsonl`, from Paris on.
            let node =
                meta["node"].as_bool().unwrap_or(true) && position(fork) >= position("Merge");
            let tracers = specs
                .keys()
                .filter(|tracer| match tracer.as_str() {
                    "result" => true,
                    "jsonl" => {
                        meta["tracers"].as_array().is_none_or(|list| list.contains(&json!("jsonl")))
                    }
                    other => {
                        node && meta["tracers"]
                            .as_array()
                            .is_none_or(|list| list.contains(&json!(other)))
                    }
                })
                .cloned()
                .collect();
            jobs.push(Job {
                case: directory.file_name().unwrap().to_str().unwrap().to_string(),
                fork: fork.to_string(),
                directory: directory.clone(),
                tracers,
            });
        }
    }
    assert!(jobs.len() > 100, "found only {} case and fork pairs", jobs.len());

    let next = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::new());
    let ran = Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism().map_or(4, usize::from);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                while let Some(job) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let found = check(job, &specs, &predeploys, &atoms, &ran);
                    failures.lock().unwrap().extend(found);
                }
            });
        }
    });

    let ran = ran.into_inner().unwrap();
    let mut failures = failures.into_inner().unwrap();
    failures.sort();
    for atom in &atoms {
        let selected = ran.iter().any(|(case, fork, tracer)| {
            glob(&atom.case, case) && glob(&atom.fork, fork) && glob(&atom.tracer, tracer)
        });
        if selected && atom.hits.load(Ordering::Relaxed) == 0 {
            failures.push(format!(
                "STALE known difference no longer occurs, delete it: {} {} {} {}",
                atom.case, atom.fork, atom.tracer, atom.path
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} unexpected differences or stale known entries; first 40:\n{}",
        failures.len(),
        failures.iter().take(40).cloned().collect::<Vec<_>>().join("\n")
    );
}
