use crate::fuzzer::{
    CaseContext,
    backend::{evm2_db, revm_db, revm_spec},
    case::{EvmCase, TxKindCase},
    normalize::{CanonicalLog, canonical_log, normalize_error},
};
use alloy_primitives::{Address, U256};
use evm2::{
    BaseEvmTypes, Evm, Inspector as Evm2Inspector, Precompiles, SpecId,
    ethereum::ethereum_tx_registry,
    interpreter::{
        InstrStop, Interpreter as Evm2Interpreter, Message as Evm2Message,
        MessageKind as Evm2MessageKind, MessageResult as Evm2MessageResult,
    },
};
use revm::{
    ExecuteCommitEvm, ExecuteEvm, InspectEvm, MainBuilder, MainContext,
    context::{CfgEnv, Context},
    context_interface::either::Either,
    database::State as RevmState,
    interpreter::{
        CallInputs, CallOutcome, CallScheme, CreateInputs, CreateOutcome, CreateScheme,
        InstructionResult, Interpreter as RevmInterpreter,
        interpreter_types::{Jumps, LegacyBytecode, LoopControl},
    },
};
use std::{cell::RefCell, rc::Rc};

const MAX_TRACE_EVENTS: usize = 100_000;

#[derive(Clone, Debug, PartialEq, Eq)]
struct InspectorOutcome {
    txs: Vec<InspectorTxTrace>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct InspectorTxTrace {
    events: Vec<TraceEvent>,
    error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NormalizedInspectorOutcome {
    txs: Vec<NormalizedTxTrace>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NormalizedTxTrace {
    events: Vec<NormalizedEvent>,
    problems: Vec<String>,
    error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum NormalizedEvent {
    Initialize { code_len: usize },
    Step { pc: usize, opcode: u8, ended: bool },
    Log(CanonicalLog),
    Frame(TraceFrame),
    SelfDestruct { contract: Address, target: Address, value: U256 },
    Truncated,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TraceFrame {
    start: TraceFrameStart,
    events: Vec<NormalizedEvent>,
    result: Option<TraceResult>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TraceFrameStart {
    Call(TraceCall),
    Create(TraceCreate),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TraceFrameKind {
    Call,
    Create,
}

impl TraceFrameStart {
    const fn kind(&self) -> TraceFrameKind {
        match self {
            Self::Call(_) => TraceFrameKind::Call,
            Self::Create(_) => TraceFrameKind::Create,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TraceEvent {
    Initialize { code_len: usize },
    Step { pc: usize, opcode: u8 },
    StepEnd,
    Log(CanonicalLog),
    CallStart(TraceCall),
    CallEnd(TraceResult),
    CreateStart(TraceCreate),
    CreateEnd(TraceResult),
    SelfDestruct { contract: Address, target: Address, value: U256 },
    Truncated,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TraceCall {
    kind: TraceCallKind,
    caller: Address,
    target: Address,
    code_address: Address,
    value: U256,
    gas_limit: u64,
    input_len: usize,
    is_static: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TraceCallKind {
    Call,
    CallCode,
    DelegateCall,
    StaticCall,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TraceCreate {
    kind: TraceCreateKind,
    caller: Address,
    value: U256,
    gas_limit: u64,
    input_len: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TraceCreateKind {
    Create,
    Create2,
    Custom,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TraceResult {
    kind: TraceResultKind,
    output_len: usize,
    created_address: Option<Address>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TraceResultKind {
    Success,
    Revert,
    Halt,
}

#[derive(Clone, Default)]
struct TraceSink {
    state: Rc<RefCell<TraceState>>,
}

#[derive(Default)]
struct TraceState {
    events: Vec<TraceEvent>,
    truncated: bool,
}

impl TraceSink {
    fn push(&self, event: TraceEvent) {
        let mut state = self.state.borrow_mut();
        if state.events.len() < MAX_TRACE_EVENTS {
            state.events.push(event);
        } else if !state.truncated {
            state.events.push(TraceEvent::Truncated);
            state.truncated = true;
        }
    }

    fn take(&self) -> Vec<TraceEvent> {
        let mut state = self.state.borrow_mut();
        state.truncated = false;
        std::mem::take(&mut state.events)
    }
}

pub fn compare_inspector_case(case: &EvmCase, context: CaseContext<'_>) -> Result<(), String> {
    let baseline = run_revm_inspector(case);
    let got = run_evm2_inspector(case);
    let mut normalized_baseline = normalize_inspector_outcome(&baseline);
    let mut normalized_got = normalize_inspector_outcome(&got);
    normalize_pairwise_truncation(&mut normalized_baseline, &mut normalized_got);
    normalize_pairwise_compatibility(&mut normalized_baseline, &mut normalized_got, case.spec);
    if normalized_got == normalized_baseline {
        return Ok(());
    }

    eprintln!("inspector differential mismatch at {context}");
    eprintln!("case:\n{case:#?}");
    eprintln!(
        "first inspector difference:\n{}",
        inspector_diff_summary(&normalized_baseline, &normalized_got)
    );
    if std::env::var_os("EVM2_FUZZ_FULL_INSPECTOR_DIFF").is_some() {
        eprintln!("normalized revm inspector:\n{normalized_baseline:#?}");
        eprintln!("normalized evm2 inspector:\n{normalized_got:#?}");
        eprintln!("raw revm inspector:\n{baseline:#?}");
        eprintln!("raw evm2 inspector:\n{got:#?}");
    } else {
        eprintln!("set EVM2_FUZZ_FULL_INSPECTOR_DIFF=1 to print full inspector traces");
    }
    Err("inspector differential mismatch".into())
}

fn inspector_diff_summary(
    baseline: &NormalizedInspectorOutcome,
    got: &NormalizedInspectorOutcome,
) -> String {
    if baseline.txs.len() != got.txs.len() {
        return format!("tx count differs: revm={} evm2={}", baseline.txs.len(), got.txs.len());
    }

    for (tx_index, (baseline_tx, got_tx)) in baseline.txs.iter().zip(&got.txs).enumerate() {
        if baseline_tx.error != got_tx.error {
            return format!(
                "tx[{tx_index}].error differs:\n  revm: {:?}\n  evm2: {:?}",
                baseline_tx.error, got_tx.error
            );
        }
        if baseline_tx.problems != got_tx.problems {
            return format!(
                "tx[{tx_index}].problems differ:\n  revm: {:?}\n  evm2: {:?}",
                baseline_tx.problems, got_tx.problems
            );
        }
        if let Some(diff) =
            first_event_diff(&format!("tx[{tx_index}].events"), &baseline_tx.events, &got_tx.events)
        {
            return diff;
        }
    }

    "normalized traces differ, but no focused difference was found".to_string()
}

fn first_event_diff(
    path: &str,
    baseline: &[NormalizedEvent],
    got: &[NormalizedEvent],
) -> Option<String> {
    let common_len = baseline.len().min(got.len());
    for index in 0..common_len {
        let event_path = format!("{path}[{index}]");
        let baseline_event = &baseline[index];
        let got_event = &got[index];
        if baseline_event == got_event {
            continue;
        }
        match (baseline_event, got_event) {
            (NormalizedEvent::Frame(baseline_frame), NormalizedEvent::Frame(got_frame)) => {
                if baseline_frame.start != got_frame.start {
                    return Some(format!(
                        "{event_path}.start differs:\n  revm: {}\n  evm2: {}",
                        describe_frame_start(&baseline_frame.start),
                        describe_frame_start(&got_frame.start)
                    ));
                }
                if let Some(diff) = first_event_diff(
                    &format!("{event_path}.events"),
                    &baseline_frame.events,
                    &got_frame.events,
                ) {
                    return Some(diff);
                }
                if baseline_frame.result != got_frame.result {
                    return Some(format!(
                        "{event_path}.result differs:\n  revm: {}\n  evm2: {}",
                        describe_optional_result(&baseline_frame.result),
                        describe_optional_result(&got_frame.result)
                    ));
                }
                return Some(format!(
                    "{event_path} differs:\n  revm: {}\n  evm2: {}",
                    describe_event(baseline_event),
                    describe_event(got_event)
                ));
            }
            _ => {
                return Some(format!(
                    "{event_path} differs:\n  revm: {}\n  evm2: {}",
                    describe_event(baseline_event),
                    describe_event(got_event)
                ));
            }
        }
    }

    if baseline.len() != got.len() {
        let baseline_extra = baseline.get(common_len).map(describe_event);
        let got_extra = got.get(common_len).map(describe_event);
        return Some(format!(
            "{path}.len differs: revm={} evm2={}\n  next revm: {}\n  next evm2: {}",
            baseline.len(),
            got.len(),
            baseline_extra.as_deref().unwrap_or("<none>"),
            got_extra.as_deref().unwrap_or("<none>")
        ));
    }

    None
}

fn describe_event(event: &NormalizedEvent) -> String {
    match event {
        NormalizedEvent::Initialize { code_len } => format!("Initialize(code_len={code_len})"),
        NormalizedEvent::Step { pc, opcode, ended } => {
            format!("Step(pc={pc}, opcode=0x{opcode:02x}, ended={ended})")
        }
        NormalizedEvent::Log(log) => format!("Log({log:?})"),
        NormalizedEvent::Frame(frame) => format!(
            "Frame(start={}, events={}, result={})",
            describe_frame_start(&frame.start),
            frame.events.len(),
            describe_optional_result(&frame.result)
        ),
        NormalizedEvent::SelfDestruct { contract, target, value } => {
            format!("SelfDestruct(contract={contract:?}, target={target:?}, value={value})")
        }
        NormalizedEvent::Truncated => "Truncated".to_string(),
    }
}

fn describe_frame_start(start: &TraceFrameStart) -> String {
    match start {
        TraceFrameStart::Call(call) => format!(
            "Call(kind={:?}, caller={:?}, target={:?}, code={:?}, value={}, gas_limit={}, input_len={}, static={})",
            call.kind,
            call.caller,
            call.target,
            call.code_address,
            call.value,
            call.gas_limit,
            call.input_len,
            call.is_static
        ),
        TraceFrameStart::Create(create) => format!(
            "Create(kind={:?}, caller={:?}, value={}, gas_limit={}, input_len={})",
            create.kind, create.caller, create.value, create.gas_limit, create.input_len
        ),
    }
}

fn describe_optional_result(result: &Option<TraceResult>) -> String {
    match result {
        Some(result) => describe_result(result),
        None => "None".to_string(),
    }
}

fn describe_result(result: &TraceResult) -> String {
    format!(
        "Some(kind={:?}, output_len={}, created_address={:?})",
        result.kind, result.output_len, result.created_address
    )
}

fn normalize_inspector_outcome(outcome: &InspectorOutcome) -> NormalizedInspectorOutcome {
    NormalizedInspectorOutcome { txs: outcome.txs.iter().map(normalize_tx_trace).collect() }
}

fn normalize_pairwise_truncation(
    baseline: &mut NormalizedInspectorOutcome,
    got: &mut NormalizedInspectorOutcome,
) {
    for (baseline_tx, got_tx) in baseline.txs.iter_mut().zip(&mut got.txs) {
        if tx_is_truncated(baseline_tx) || tx_is_truncated(got_tx) {
            baseline_tx.events.clear();
            baseline_tx.events.push(NormalizedEvent::Truncated);
            baseline_tx.problems.clear();
            got_tx.events.clear();
            got_tx.events.push(NormalizedEvent::Truncated);
            got_tx.problems.clear();
        }
    }
}

fn normalize_pairwise_compatibility(
    baseline: &mut NormalizedInspectorOutcome,
    got: &mut NormalizedInspectorOutcome,
    spec: SpecId,
) {
    for (baseline_tx, got_tx) in baseline.txs.iter_mut().zip(&mut got.txs) {
        normalize_pairwise_events(&mut baseline_tx.events, &mut got_tx.events, spec);
    }
}

fn normalize_pairwise_events(
    baseline: &mut Vec<NormalizedEvent>,
    got: &mut Vec<NormalizedEvent>,
    spec: SpecId,
) {
    let mut index = 0;
    while index < baseline.len() && index < got.len() {
        let baseline_is_selfdestruct =
            matches!(baseline[index], NormalizedEvent::SelfDestruct { .. });
        let got_is_selfdestruct = matches!(got[index], NormalizedEvent::SelfDestruct { .. });
        if baseline_is_selfdestruct && got_is_selfdestruct {
            index += 1;
            continue;
        }
        if is_ignorable_selfdestruct(&baseline[index], spec) {
            baseline.remove(index);
            continue;
        }
        if is_ignorable_selfdestruct(&got[index], spec) {
            got.remove(index);
            continue;
        }

        if let (NormalizedEvent::Frame(baseline_frame), NormalizedEvent::Frame(got_frame)) =
            (&mut baseline[index], &mut got[index])
        {
            normalize_pairwise_frame(baseline_frame, got_frame, spec);
        }
        index += 1;
    }

    while index < baseline.len() && is_ignorable_selfdestruct(&baseline[index], spec) {
        baseline.remove(index);
    }
    while index < got.len() && is_ignorable_selfdestruct(&got[index], spec) {
        got.remove(index);
    }
}

fn normalize_pairwise_frame(baseline: &mut TraceFrame, got: &mut TraceFrame, spec: SpecId) {
    normalize_pairwise_events(&mut baseline.events, &mut got.events, spec);

    if !matches!(baseline.start, TraceFrameStart::Create(_))
        || !matches!(got.start, TraceFrameStart::Create(_))
    {
        return;
    }
    let (Some(baseline_result), Some(got_result)) = (&mut baseline.result, &mut got.result) else {
        return;
    };
    if baseline_result.kind != got_result.kind
        || baseline_result.kind == TraceResultKind::Success
        || baseline_result.created_address.is_none() == got_result.created_address.is_none()
    {
        return;
    }
    baseline_result.created_address = None;
    got_result.created_address = None;
}

fn is_ignorable_selfdestruct(event: &NormalizedEvent, spec: SpecId) -> bool {
    spec.enables(SpecId::CANCUN)
        && matches!(
            event,
            NormalizedEvent::SelfDestruct { contract, target, .. } if contract == target
        )
}

fn tx_is_truncated(tx: &NormalizedTxTrace) -> bool {
    matches!(tx.events.as_slice(), [NormalizedEvent::Truncated])
}

fn normalize_tx_trace(tx: &InspectorTxTrace) -> NormalizedTxTrace {
    let mut normalizer = TraceNormalizer::new(tx.error.clone());
    for event in &tx.events {
        normalizer.push(event.clone());
    }
    normalizer.finish()
}

struct TraceNormalizer {
    tx: NormalizedTxTrace,
    stack: Vec<TraceFrame>,
}

impl TraceNormalizer {
    const fn new(error: Option<String>) -> Self {
        Self {
            tx: NormalizedTxTrace { events: Vec::new(), problems: Vec::new(), error },
            stack: Vec::new(),
        }
    }

    fn push(&mut self, event: TraceEvent) {
        match event {
            TraceEvent::Initialize { code_len } => {
                self.push_event(NormalizedEvent::Initialize { code_len });
            }
            TraceEvent::Step { pc, opcode } => self.push_step(pc, opcode),
            TraceEvent::StepEnd => self.mark_step_end(),
            TraceEvent::Log(log) => self.push_event(NormalizedEvent::Log(log)),
            TraceEvent::CallStart(call) => self.start_frame(TraceFrameStart::Call(call)),
            TraceEvent::CallEnd(result) => self.end_frame(TraceFrameKind::Call, result),
            TraceEvent::CreateStart(create) => self.start_frame(TraceFrameStart::Create(create)),
            TraceEvent::CreateEnd(result) => self.end_frame(TraceFrameKind::Create, result),
            TraceEvent::SelfDestruct { contract, target, value } => {
                self.push_event(NormalizedEvent::SelfDestruct { contract, target, value });
            }
            TraceEvent::Truncated => self.push_event(NormalizedEvent::Truncated),
        }
    }

    fn finish(mut self) -> NormalizedTxTrace {
        while let Some(frame) = self.stack.pop() {
            self.tx.problems.push(format!("unterminated frame: {:?}", frame.start));
            self.attach_frame(frame);
        }
        if events_contain_truncated(&self.tx.events) {
            self.tx.events.clear();
            self.tx.events.push(NormalizedEvent::Truncated);
            self.tx.problems.clear();
        }
        self.tx
    }

    fn current_events_mut(&mut self) -> &mut Vec<NormalizedEvent> {
        if let Some(frame) = self.stack.last_mut() {
            &mut frame.events
        } else {
            &mut self.tx.events
        }
    }

    fn push_event(&mut self, event: NormalizedEvent) {
        self.current_events_mut().push(event);
    }

    fn push_step(&mut self, pc: usize, opcode: u8) {
        if Self::last_unended_step_mut(self.current_events_mut()).is_some() {
            self.tx.problems.push("step started before previous step_end".to_string());
        }
        self.push_event(NormalizedEvent::Step { pc, opcode, ended: false });
    }

    fn mark_step_end(&mut self) {
        if let Some(ended) = Self::last_unended_step_mut(self.current_events_mut()) {
            *ended = true;
        } else {
            self.tx.problems.push("step_end without active step".to_string());
        }
    }

    fn start_frame(&mut self, start: TraceFrameStart) {
        self.stack.push(TraceFrame { start, events: Vec::new(), result: None });
    }

    fn end_frame(&mut self, expected: TraceFrameKind, result: TraceResult) {
        let Some(mut frame) = self.stack.pop() else {
            self.tx.problems.push(format!("{expected:?} end without active frame"));
            self.push_event(NormalizedEvent::Frame(TraceFrame {
                start: placeholder_frame_start(expected),
                events: Vec::new(),
                result: Some(result),
            }));
            return;
        };

        let actual = frame.start.kind();
        if actual != expected {
            self.tx.problems.push(format!("{expected:?} end closed {actual:?} frame"));
        }
        frame.result = Some(result);
        self.attach_frame(frame);
    }

    fn attach_frame(&mut self, mut frame: TraceFrame) {
        normalize_child_frame_step_ends(&mut frame);
        normalize_terminal_frame(&mut frame);
        normalize_empty_call_frame(&mut frame);
        self.push_event(NormalizedEvent::Frame(frame));
    }

    fn last_unended_step_mut(events: &mut [NormalizedEvent]) -> Option<&mut bool> {
        events.iter_mut().rev().find_map(|event| match event {
            NormalizedEvent::Step { ended, .. } if !*ended => Some(ended),
            _ => None,
        })
    }
}

fn events_contain_truncated(events: &[NormalizedEvent]) -> bool {
    events.iter().any(|event| match event {
        NormalizedEvent::Truncated => true,
        NormalizedEvent::Frame(frame) => events_contain_truncated(&frame.events),
        _ => false,
    })
}

fn normalize_child_frame_step_ends(frame: &mut TraceFrame) {
    for index in 0..frame.events.len().saturating_sub(1) {
        let has_child_frame =
            matches!(frame.events.get(index + 1), Some(NormalizedEvent::Frame(_)));
        if !has_child_frame {
            continue;
        }
        if let NormalizedEvent::Step { opcode, ended, .. } = &mut frame.events[index]
            && !*ended
            && is_frame_opcode(*opcode)
        {
            *ended = true;
        }
    }
}

const fn is_frame_opcode(opcode: u8) -> bool {
    matches!(opcode, 0xf0 | 0xf1 | 0xf2 | 0xf4 | 0xf5 | 0xfa)
}

fn normalize_terminal_frame(frame: &mut TraceFrame) {
    let code_len = frame.events.iter().find_map(|event| match event {
        NormalizedEvent::Initialize { code_len } => Some(*code_len),
        _ => None,
    });
    if let Some(code_len) = code_len
        && matches!(
            frame.events.last(),
            Some(NormalizedEvent::Step { pc, opcode: 0x00, ended: true }) if *pc >= code_len
        )
    {
        frame.events.pop();
    }

    let last_unended_step = frame
        .events
        .iter()
        .rposition(|event| matches!(event, NormalizedEvent::Step { ended: false, .. }));
    if let Some(index) = last_unended_step {
        let only_terminal_hooks_after = frame.events[index + 1..].iter().all(|event| {
            matches!(event, NormalizedEvent::Log(_) | NormalizedEvent::SelfDestruct { .. })
        });
        if only_terminal_hooks_after {
            let NormalizedEvent::Step { ended, .. } = &mut frame.events[index] else {
                unreachable!();
            };
            *ended = true;
        }
    }
}

fn normalize_empty_call_frame(frame: &mut TraceFrame) {
    if !matches!(frame.start, TraceFrameStart::Call(_)) {
        return;
    }
    let Some(result) = &frame.result else {
        return;
    };
    if result.kind != TraceResultKind::Success
        || result.output_len != 0
        || result.created_address.is_some()
    {
        return;
    }
    if frame.events.iter().all(|event| {
        matches!(event, NormalizedEvent::Initialize { code_len: 0 } | NormalizedEvent::Log(_))
    }) {
        frame.events.retain(|event| !matches!(event, NormalizedEvent::Initialize { code_len: 0 }));
    }
}

const fn placeholder_frame_start(kind: TraceFrameKind) -> TraceFrameStart {
    match kind {
        TraceFrameKind::Call => TraceFrameStart::Call(TraceCall {
            kind: TraceCallKind::Call,
            caller: Address::ZERO,
            target: Address::ZERO,
            code_address: Address::ZERO,
            value: U256::ZERO,
            gas_limit: 0,
            input_len: 0,
            is_static: false,
        }),
        TraceFrameKind::Create => TraceFrameStart::Create(TraceCreate {
            kind: TraceCreateKind::Custom,
            caller: Address::ZERO,
            value: U256::ZERO,
            gas_limit: 0,
            input_len: 0,
        }),
    }
}

fn run_evm2_inspector(case: &EvmCase) -> InspectorOutcome {
    let sink = TraceSink::default();
    let mut evm = Evm::<BaseEvmTypes>::new(
        case.spec,
        case.block.evm2(),
        ethereum_tx_registry(case.spec),
        evm2_db(case),
        Precompiles::base(case.spec),
    );
    evm.set_inspector(Evm2TraceInspector { sink: sink.clone() });

    let mut txs = Vec::new();
    for tx in case.txs() {
        let result = evm
            .transact(&tx.evm2())
            .map(|executed| executed.detach())
            .map_err(|err| normalize_error(format!("{err:?}")));
        // A resolved top-level transaction must clear all transaction-local state, or warm/touched
        // entries can leak into the next transaction and change EIP-2929 gas semantics.
        assert!(
            evm.state().transaction_state_is_empty(),
            "evm2 inspect run left transaction-local state behind"
        );
        let events = sink.take();
        match result {
            Ok(result) => {
                evm.commit_source(&result.pending_state);
                txs.push(InspectorTxTrace { events, error: None });
            }
            Err(err) => {
                txs.push(InspectorTxTrace { events, error: Some(err) });
                break;
            }
        }
    }
    InspectorOutcome { txs }
}

fn run_revm_inspector(case: &EvmCase) -> InspectorOutcome {
    let sink = TraceSink::default();
    let mut cfg = CfgEnv::new();
    cfg.set_spec_and_mainnet_gas_params(revm_spec(case.spec));
    cfg = cfg.disable_tx_chain_id_check();
    let mut evm = Context::mainnet()
        .with_cfg(cfg)
        .with_block(case.block.revm())
        .with_db(RevmState::builder().with_database(revm_db(case)).build())
        .build_mainnet_with_inspector(RevmTraceInspector { sink: sink.clone() });

    let mut txs = Vec::new();
    for tx in case.txs() {
        let mut tx_env = tx.revm();
        if tx.kind == TxKindCase::Eip7702 {
            tx_env.authorization_list =
                tx.eip7702_authorization_list().into_iter().map(Either::Left).collect();
        }
        let result = evm.inspect_tx(tx_env).map_err(|err| normalize_error(format!("{err:?}")));
        let leftover = evm.finalize();
        // Revm inspect_tx must finalize even on errors; a second finalize must not drain anything,
        // or warmed/touched journal state can leak into the next inspected transaction.
        assert!(
            leftover.is_empty(),
            "revm inspect_tx left transaction-local journal state: {leftover:#?}"
        );
        let events = sink.take();
        match result {
            Ok(result) => {
                evm.commit(result.state);
                txs.push(InspectorTxTrace { events, error: None });
            }
            Err(err) => {
                txs.push(InspectorTxTrace { events, error: Some(err) });
                break;
            }
        }
    }
    InspectorOutcome { txs }
}

#[derive(Clone)]
struct Evm2TraceInspector {
    sink: TraceSink,
}

impl Evm2Inspector<BaseEvmTypes> for Evm2TraceInspector {
    fn initialize_interp(&mut self, interp: &mut Evm2Interpreter<'_, '_, BaseEvmTypes>) {
        self.sink.push(TraceEvent::Initialize { code_len: interp.bytecode().len() });
    }

    fn step(&mut self, interp: &mut Evm2Interpreter<'_, '_, BaseEvmTypes>) {
        if interp.pc() < interp.bytecode().len() {
            self.sink.push(TraceEvent::Step { pc: interp.pc(), opcode: interp.opcode() });
        }
    }

    fn step_end(&mut self, interp: &mut Evm2Interpreter<'_, '_, BaseEvmTypes>) {
        if interp.pc() < interp.bytecode().len() {
            self.sink.push(TraceEvent::StepEnd);
        }
    }

    fn log(&mut self, log: &alloy_primitives::Log, _host: &mut Evm<'_, BaseEvmTypes>) {
        self.sink.push(TraceEvent::Log(canonical_log(log)));
    }

    fn call(
        &mut self,
        _interp: &mut Evm2Interpreter<'_, '_, BaseEvmTypes>,
        message: &mut Evm2Message<BaseEvmTypes>,
    ) -> Option<Evm2MessageResult<BaseEvmTypes>> {
        self.sink.push(TraceEvent::CallStart(evm2_call(message)));
        None
    }

    fn call_end(
        &mut self,
        _interp: &mut Evm2Interpreter<'_, '_, BaseEvmTypes>,
        _message: &Evm2Message<BaseEvmTypes>,
        result: &mut Evm2MessageResult<BaseEvmTypes>,
    ) {
        self.sink.push(TraceEvent::CallEnd(evm2_result(result)));
    }

    fn create(
        &mut self,
        _interp: &mut Evm2Interpreter<'_, '_, BaseEvmTypes>,
        message: &mut Evm2Message<BaseEvmTypes>,
    ) -> Option<Evm2MessageResult<BaseEvmTypes>> {
        self.sink.push(TraceEvent::CreateStart(evm2_create(message)));
        None
    }

    fn create_end(
        &mut self,
        _interp: &mut Evm2Interpreter<'_, '_, BaseEvmTypes>,
        _message: &Evm2Message<BaseEvmTypes>,
        result: &mut Evm2MessageResult<BaseEvmTypes>,
    ) {
        self.sink.push(TraceEvent::CreateEnd(evm2_result(result)));
    }

    fn selfdestruct(
        &mut self,
        contract: &Address,
        target: &Address,
        value: &U256,
        _host: &mut Evm<'_, BaseEvmTypes>,
    ) {
        self.sink.push(TraceEvent::SelfDestruct {
            contract: *contract,
            target: *target,
            value: *value,
        });
    }
}

#[derive(Clone)]
struct RevmTraceInspector {
    sink: TraceSink,
}

impl<CTX> revm::Inspector<CTX> for RevmTraceInspector {
    fn initialize_interp(&mut self, interp: &mut RevmInterpreter, _context: &mut CTX) {
        self.sink.push(TraceEvent::Initialize { code_len: interp.bytecode.bytecode_len() });
    }

    fn step(&mut self, interp: &mut RevmInterpreter, _context: &mut CTX) {
        if !interp.bytecode.is_end() {
            self.sink.push(TraceEvent::Step {
                pc: interp.bytecode.pc(),
                opcode: interp.bytecode.opcode(),
            });
        }
    }

    fn step_end(&mut self, _interp: &mut RevmInterpreter, _context: &mut CTX) {
        self.sink.push(TraceEvent::StepEnd);
    }

    fn log(&mut self, _context: &mut CTX, log: alloy_primitives::Log) {
        self.sink.push(TraceEvent::Log(canonical_log(&log)));
    }

    fn call(&mut self, _context: &mut CTX, inputs: &mut CallInputs) -> Option<CallOutcome> {
        self.sink.push(TraceEvent::CallStart(revm_call(inputs)));
        None
    }

    fn call_end(&mut self, _context: &mut CTX, _inputs: &CallInputs, outcome: &mut CallOutcome) {
        self.sink.push(TraceEvent::CallEnd(revm_call_result(outcome)));
    }

    fn create(&mut self, _context: &mut CTX, inputs: &mut CreateInputs) -> Option<CreateOutcome> {
        self.sink.push(TraceEvent::CreateStart(revm_create(inputs)));
        None
    }

    fn create_end(
        &mut self,
        _context: &mut CTX,
        _inputs: &CreateInputs,
        outcome: &mut CreateOutcome,
    ) {
        self.sink.push(TraceEvent::CreateEnd(revm_create_result(outcome)));
    }

    fn selfdestruct(&mut self, contract: Address, target: Address, value: U256) {
        self.sink.push(TraceEvent::SelfDestruct { contract, target, value });
    }
}

fn evm2_call(message: &Evm2Message<BaseEvmTypes>) -> TraceCall {
    TraceCall {
        kind: evm2_call_kind(message.kind),
        caller: message.caller,
        target: message.destination,
        code_address: message.code_address,
        value: message.value,
        gas_limit: message.gas_limit,
        input_len: message.input.len(),
        is_static: message.caller_is_static || message.kind == Evm2MessageKind::StaticCall,
    }
}

fn evm2_create(message: &Evm2Message<BaseEvmTypes>) -> TraceCreate {
    TraceCreate {
        kind: match message.kind {
            Evm2MessageKind::Create => TraceCreateKind::Create,
            Evm2MessageKind::Create2 => TraceCreateKind::Create2,
            _ => TraceCreateKind::Custom,
        },
        caller: message.caller,
        value: message.value,
        gas_limit: message.gas_limit,
        input_len: message.input.len(),
    }
}

const fn evm2_call_kind(kind: Evm2MessageKind) -> TraceCallKind {
    match kind {
        Evm2MessageKind::Call => TraceCallKind::Call,
        Evm2MessageKind::CallCode => TraceCallKind::CallCode,
        Evm2MessageKind::DelegateCall => TraceCallKind::DelegateCall,
        Evm2MessageKind::StaticCall => TraceCallKind::StaticCall,
        Evm2MessageKind::Create | Evm2MessageKind::Create2 => TraceCallKind::Call,
        _ => TraceCallKind::Call,
    }
}

fn evm2_result(result: &Evm2MessageResult<BaseEvmTypes>) -> TraceResult {
    TraceResult {
        kind: evm2_result_kind(result.stop),
        output_len: result.output.len(),
        created_address: result.created_address,
    }
}

const fn evm2_result_kind(stop: InstrStop) -> TraceResultKind {
    if stop.is_success() {
        TraceResultKind::Success
    } else if stop.is_revert() {
        TraceResultKind::Revert
    } else {
        TraceResultKind::Halt
    }
}

fn revm_call(inputs: &CallInputs) -> TraceCall {
    TraceCall {
        kind: revm_call_kind(inputs.scheme),
        caller: inputs.caller,
        target: inputs.target_address,
        code_address: inputs.bytecode_address,
        value: inputs.call_value(),
        gas_limit: inputs.gas_limit,
        input_len: inputs.input.len(),
        is_static: inputs.is_static,
    }
}

fn revm_create(inputs: &CreateInputs) -> TraceCreate {
    TraceCreate {
        kind: revm_create_kind(inputs.scheme()),
        caller: inputs.caller(),
        value: inputs.value(),
        gas_limit: inputs.gas_limit(),
        input_len: inputs.init_code().len(),
    }
}

const fn revm_call_kind(kind: CallScheme) -> TraceCallKind {
    match kind {
        CallScheme::Call => TraceCallKind::Call,
        CallScheme::CallCode => TraceCallKind::CallCode,
        CallScheme::DelegateCall => TraceCallKind::DelegateCall,
        CallScheme::StaticCall => TraceCallKind::StaticCall,
    }
}

const fn revm_create_kind(kind: CreateScheme) -> TraceCreateKind {
    match kind {
        CreateScheme::Create => TraceCreateKind::Create,
        CreateScheme::Create2 { .. } => TraceCreateKind::Create2,
        CreateScheme::Custom { .. } => TraceCreateKind::Custom,
    }
}

fn revm_call_result(outcome: &CallOutcome) -> TraceResult {
    TraceResult {
        kind: revm_result_kind(outcome.result.result),
        output_len: outcome.output().len(),
        created_address: None,
    }
}

fn revm_create_result(outcome: &CreateOutcome) -> TraceResult {
    TraceResult {
        kind: revm_result_kind(outcome.result.result),
        output_len: outcome.output().len(),
        created_address: outcome.address,
    }
}

const fn revm_result_kind(result: InstructionResult) -> TraceResultKind {
    if result.is_ok() {
        TraceResultKind::Success
    } else if result.is_revert() {
        TraceResultKind::Revert
    } else {
        TraceResultKind::Halt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_parent_step_end_around_child_call_frame() {
        let parent = call_trace(100);
        let child = call_trace(10);
        let child_result = trace_result(TraceResultKind::Revert);
        let parent_result = trace_result(TraceResultKind::Success);

        let revm_order = normalize_events(vec![
            TraceEvent::CallStart(parent.clone()),
            initialize(2),
            TraceEvent::Step { pc: 47, opcode: 0xf1 },
            TraceEvent::StepEnd,
            TraceEvent::CallStart(child.clone()),
            TraceEvent::CallEnd(child_result.clone()),
            TraceEvent::Step { pc: 48, opcode: 0x50 },
            TraceEvent::StepEnd,
            TraceEvent::CallEnd(parent_result.clone()),
        ]);
        let evm2_order = normalize_events(vec![
            TraceEvent::CallStart(parent),
            initialize(2),
            TraceEvent::Step { pc: 47, opcode: 0xf1 },
            TraceEvent::CallStart(child),
            TraceEvent::CallEnd(child_result),
            TraceEvent::StepEnd,
            TraceEvent::Step { pc: 48, opcode: 0x50 },
            TraceEvent::StepEnd,
            TraceEvent::CallEnd(parent_result),
        ]);

        assert_eq!(revm_order, evm2_order);
    }

    #[test]
    fn normalizes_parent_call_step_end_when_child_frame_exists() {
        let parent = call_trace(100);
        let child = call_trace(10);
        let child_result = trace_result(TraceResultKind::Revert);
        let parent_result = trace_result(TraceResultKind::Success);

        let revm_order = normalize_events(vec![
            TraceEvent::CallStart(parent.clone()),
            initialize(2),
            TraceEvent::Step { pc: 47, opcode: 0xf1 },
            TraceEvent::StepEnd,
            TraceEvent::CallStart(child.clone()),
            TraceEvent::CallEnd(child_result.clone()),
            TraceEvent::CallEnd(parent_result.clone()),
        ]);
        let evm2_order = normalize_events(vec![
            TraceEvent::CallStart(parent),
            initialize(2),
            TraceEvent::Step { pc: 47, opcode: 0xf1 },
            TraceEvent::CallStart(child),
            TraceEvent::CallEnd(child_result),
            TraceEvent::CallEnd(parent_result),
        ]);

        assert_eq!(revm_order, evm2_order);
    }

    #[test]
    fn normalized_trace_still_compares_child_call_metadata() {
        let parent = call_trace(100);
        let child_result = trace_result(TraceResultKind::Revert);
        let parent_result = trace_result(TraceResultKind::Success);

        let baseline = normalize_events(vec![
            TraceEvent::CallStart(parent.clone()),
            TraceEvent::Step { pc: 47, opcode: 0xf1 },
            TraceEvent::StepEnd,
            TraceEvent::CallStart(call_trace(10)),
            TraceEvent::CallEnd(child_result.clone()),
            TraceEvent::CallEnd(parent_result.clone()),
        ]);
        let got = normalize_events(vec![
            TraceEvent::CallStart(parent),
            TraceEvent::Step { pc: 47, opcode: 0xf1 },
            TraceEvent::CallStart(call_trace(11)),
            TraceEvent::CallEnd(child_result),
            TraceEvent::StepEnd,
            TraceEvent::CallEnd(parent_result),
        ]);

        assert_ne!(baseline, got);
    }

    #[test]
    fn normalizes_truncated_trace_boundary() {
        let call = call_trace(100);
        let revm = normalize_events(vec![
            TraceEvent::CallStart(call.clone()),
            initialize(8),
            TraceEvent::Step { pc: 4, opcode: 0x60 },
            TraceEvent::Truncated,
        ]);
        let evm2 = normalize_events(vec![
            TraceEvent::CallStart(call),
            initialize(8),
            TraceEvent::Step { pc: 4, opcode: 0x60 },
            TraceEvent::StepEnd,
            TraceEvent::Truncated,
        ]);

        assert_eq!(revm, evm2);
        assert_eq!(revm.events, vec![NormalizedEvent::Truncated]);
        assert!(revm.problems.is_empty());
    }

    #[test]
    fn normalizes_one_sided_trace_truncation() {
        let call = call_trace(100);
        let result = trace_result(TraceResultKind::Success);
        let mut revm = NormalizedInspectorOutcome {
            txs: vec![normalize_events(vec![
                TraceEvent::CallStart(call.clone()),
                initialize(8),
                TraceEvent::Step { pc: 4, opcode: 0x60 },
                TraceEvent::Truncated,
            ])],
        };
        let mut evm2 = NormalizedInspectorOutcome {
            txs: vec![normalize_events(vec![
                TraceEvent::CallStart(call),
                initialize(8),
                TraceEvent::Step { pc: 0, opcode: 0x00 },
                TraceEvent::StepEnd,
                TraceEvent::CallEnd(result),
            ])],
        };

        assert_ne!(revm, evm2);
        normalize_pairwise_truncation(&mut revm, &mut evm2);
        assert_eq!(revm, evm2);
        assert_eq!(revm.txs[0].events, vec![NormalizedEvent::Truncated]);
        assert!(evm2.txs[0].problems.is_empty());
    }

    #[test]
    fn normalizes_empty_call_frame_initialize() {
        let empty_call = call_trace(500_000);
        let result = trace_result(TraceResultKind::Success);

        let revm_empty = normalize_events(vec![
            TraceEvent::CallStart(empty_call.clone()),
            TraceEvent::CallEnd(result.clone()),
        ]);
        let evm2_empty = normalize_events(vec![
            TraceEvent::CallStart(empty_call),
            initialize(0),
            TraceEvent::CallEnd(result),
        ]);

        assert_eq!(revm_empty, evm2_empty);
    }

    #[test]
    fn normalizes_empty_call_frame_initialize_after_transfer_log() {
        let empty_call = call_trace(500_000);
        let result = trace_result(TraceResultKind::Success);
        let log = CanonicalLog { address: Address::ZERO, topics: Vec::new(), data: vec![1] };

        let revm_empty = normalize_events(vec![
            TraceEvent::CallStart(empty_call.clone()),
            TraceEvent::Log(log.clone()),
            TraceEvent::CallEnd(result.clone()),
        ]);
        let evm2_empty = normalize_events(vec![
            TraceEvent::CallStart(empty_call),
            TraceEvent::Log(log),
            initialize(0),
            TraceEvent::CallEnd(result),
        ]);

        assert_eq!(revm_empty, evm2_empty);
    }

    #[test]
    fn initialize_is_kept_for_non_empty_call_frame() {
        let call = call_trace(500_000);
        let result = trace_result(TraceResultKind::Success);

        let without_initialize = normalize_events(vec![
            TraceEvent::CallStart(call.clone()),
            TraceEvent::Step { pc: 0, opcode: 0x00 },
            TraceEvent::StepEnd,
            TraceEvent::CallEnd(result.clone()),
        ]);
        let with_initialize = normalize_events(vec![
            TraceEvent::CallStart(call),
            initialize(1),
            TraceEvent::Step { pc: 0, opcode: 0x00 },
            TraceEvent::StepEnd,
            TraceEvent::CallEnd(result),
        ]);

        assert_ne!(without_initialize, with_initialize);
    }

    #[test]
    fn normalizes_revm_eof_stop_at_code_end() {
        let call = call_trace(500_000);
        let result = trace_result(TraceResultKind::Success);

        let revm = normalize_events(vec![
            TraceEvent::CallStart(call.clone()),
            initialize(1),
            TraceEvent::Step { pc: 0, opcode: 0x61 },
            TraceEvent::StepEnd,
            TraceEvent::Step { pc: 1, opcode: 0x00 },
            TraceEvent::StepEnd,
            TraceEvent::CallEnd(result.clone()),
        ]);
        let evm2 = normalize_events(vec![
            TraceEvent::CallStart(call),
            initialize(1),
            TraceEvent::Step { pc: 0, opcode: 0x61 },
            TraceEvent::CallEnd(result),
        ]);

        assert_eq!(revm, evm2);
    }

    #[test]
    fn normalizes_terminal_log_step_end_before_frame_end() {
        let call = call_trace(500_000);
        let result = trace_result(TraceResultKind::Success);
        let log = CanonicalLog { address: Address::ZERO, topics: Vec::new(), data: Vec::new() };

        let revm = normalize_events(vec![
            TraceEvent::CallStart(call.clone()),
            initialize(4),
            TraceEvent::Step { pc: 3, opcode: 0xa0 },
            TraceEvent::Log(log.clone()),
            TraceEvent::StepEnd,
            TraceEvent::Step { pc: 4, opcode: 0x00 },
            TraceEvent::StepEnd,
            TraceEvent::CallEnd(result.clone()),
        ]);
        let evm2 = normalize_events(vec![
            TraceEvent::CallStart(call),
            initialize(4),
            TraceEvent::Step { pc: 3, opcode: 0xa0 },
            TraceEvent::Log(log),
            TraceEvent::CallEnd(result),
        ]);

        assert_eq!(revm, evm2);
    }

    #[test]
    fn normalizes_revm_padded_eof_stop_after_truncated_push() {
        let call = call_trace(500_000);
        let result = trace_result(TraceResultKind::Success);

        let revm = normalize_events(vec![
            TraceEvent::CallStart(call.clone()),
            initialize(2),
            TraceEvent::Step { pc: 0, opcode: 0x77 },
            TraceEvent::StepEnd,
            TraceEvent::Step { pc: 25, opcode: 0x00 },
            TraceEvent::StepEnd,
            TraceEvent::CallEnd(result.clone()),
        ]);
        let evm2 = normalize_events(vec![
            TraceEvent::CallStart(call),
            initialize(2),
            TraceEvent::Step { pc: 0, opcode: 0x77 },
            TraceEvent::CallEnd(result),
        ]);

        assert_eq!(revm, evm2);
    }

    #[test]
    fn keeps_real_stop_before_code_end() {
        let call = call_trace(500_000);
        let result = trace_result(TraceResultKind::Success);

        let with_real_stop = normalize_events(vec![
            TraceEvent::CallStart(call.clone()),
            initialize(2),
            TraceEvent::Step { pc: 0, opcode: 0x5f },
            TraceEvent::StepEnd,
            TraceEvent::Step { pc: 1, opcode: 0x00 },
            TraceEvent::StepEnd,
            TraceEvent::CallEnd(result.clone()),
        ]);
        let without_real_stop = normalize_events(vec![
            TraceEvent::CallStart(call),
            initialize(2),
            TraceEvent::Step { pc: 0, opcode: 0x5f },
            TraceEvent::StepEnd,
            TraceEvent::CallEnd(result),
        ]);

        assert_ne!(with_real_stop, without_real_stop);
    }

    #[test]
    fn normalizes_empty_create_eof_stop() {
        let create = create_trace(12_535, 0);
        let created = Address::repeat_byte(0x11);
        let result = trace_create_result(TraceResultKind::Success, Some(created));

        let revm = normalize_events(vec![
            TraceEvent::CreateStart(create.clone()),
            initialize(0),
            TraceEvent::Step { pc: 0, opcode: 0x00 },
            TraceEvent::StepEnd,
            TraceEvent::CreateEnd(result.clone()),
        ]);
        let evm2 = normalize_events(vec![
            TraceEvent::CreateStart(create),
            initialize(0),
            TraceEvent::CreateEnd(result),
        ]);

        assert_eq!(revm, evm2);
    }

    #[test]
    fn normalizes_one_sided_non_success_create_address() {
        let create = create_trace(12_535, 0);
        let created = Address::repeat_byte(0x11);

        let (revm, evm2) = normalize_event_pair(
            SpecId::BERLIN,
            vec![
                TraceEvent::CreateStart(create.clone()),
                TraceEvent::CreateEnd(trace_create_result(TraceResultKind::Halt, Some(created))),
            ],
            vec![
                TraceEvent::CreateStart(create),
                TraceEvent::CreateEnd(trace_create_result(TraceResultKind::Halt, None)),
            ],
        );

        assert_eq!(revm, evm2);
    }

    #[test]
    fn compares_non_success_create_addresses_when_both_are_present() {
        let create = create_trace(12_535, 0);

        let (revm, evm2) = normalize_event_pair(
            SpecId::BERLIN,
            vec![
                TraceEvent::CreateStart(create.clone()),
                TraceEvent::CreateEnd(trace_create_result(
                    TraceResultKind::Halt,
                    Some(Address::repeat_byte(0x11)),
                )),
            ],
            vec![
                TraceEvent::CreateStart(create),
                TraceEvent::CreateEnd(trace_create_result(
                    TraceResultKind::Halt,
                    Some(Address::repeat_byte(0x22)),
                )),
            ],
        );

        assert_ne!(revm, evm2);
    }

    #[test]
    fn normalizes_cancun_noop_selfdestruct_hook_presence() {
        let call = call_trace(500_000);
        let result = trace_result(TraceResultKind::Success);
        let target = Address::repeat_byte(0x22);

        let (revm, evm2) = normalize_event_pair(
            SpecId::CANCUN,
            vec![
                TraceEvent::CallStart(call.clone()),
                initialize(1),
                TraceEvent::Step { pc: 0, opcode: 0xff },
                TraceEvent::StepEnd,
                TraceEvent::CallEnd(result.clone()),
            ],
            vec![
                TraceEvent::CallStart(call),
                initialize(1),
                TraceEvent::Step { pc: 0, opcode: 0xff },
                TraceEvent::SelfDestruct { contract: target, target, value: U256::from(1_u64) },
                TraceEvent::StepEnd,
                TraceEvent::CallEnd(result),
            ],
        );

        assert_eq!(revm, evm2);
    }

    #[test]
    fn compares_selfdestruct_metadata_when_both_hooks_are_present() {
        let call = call_trace(500_000);
        let result = trace_result(TraceResultKind::Success);
        let target = Address::repeat_byte(0x22);

        let (revm, evm2) = normalize_event_pair(
            SpecId::CANCUN,
            vec![
                TraceEvent::CallStart(call.clone()),
                initialize(1),
                TraceEvent::Step { pc: 0, opcode: 0xff },
                TraceEvent::SelfDestruct { contract: target, target, value: U256::from(1_u64) },
                TraceEvent::StepEnd,
                TraceEvent::CallEnd(result.clone()),
            ],
            vec![
                TraceEvent::CallStart(call),
                initialize(1),
                TraceEvent::Step { pc: 0, opcode: 0xff },
                TraceEvent::SelfDestruct { contract: target, target, value: U256::from(2_u64) },
                TraceEvent::StepEnd,
                TraceEvent::CallEnd(result),
            ],
        );

        assert_ne!(revm, evm2);
    }

    #[test]
    fn compares_non_noop_selfdestruct_hook_presence() {
        let call = call_trace(500_000);
        let result = trace_result(TraceResultKind::Success);
        let contract = Address::repeat_byte(0x22);
        let target = Address::repeat_byte(0x33);

        let (revm, evm2) = normalize_event_pair(
            SpecId::CANCUN,
            vec![
                TraceEvent::CallStart(call.clone()),
                initialize(1),
                TraceEvent::Step { pc: 0, opcode: 0xff },
                TraceEvent::StepEnd,
                TraceEvent::CallEnd(result.clone()),
            ],
            vec![
                TraceEvent::CallStart(call),
                initialize(1),
                TraceEvent::Step { pc: 0, opcode: 0xff },
                TraceEvent::SelfDestruct { contract, target, value: U256::from(1_u64) },
                TraceEvent::StepEnd,
                TraceEvent::CallEnd(result),
            ],
        );

        assert_ne!(revm, evm2);
    }

    #[test]
    fn compares_pre_cancun_selfdestruct_hook_presence() {
        let call = call_trace(500_000);
        let result = trace_result(TraceResultKind::Success);
        let target = Address::repeat_byte(0x22);

        let (revm, evm2) = normalize_event_pair(
            SpecId::SHANGHAI,
            vec![
                TraceEvent::CallStart(call.clone()),
                initialize(1),
                TraceEvent::Step { pc: 0, opcode: 0xff },
                TraceEvent::StepEnd,
                TraceEvent::CallEnd(result.clone()),
            ],
            vec![
                TraceEvent::CallStart(call),
                initialize(1),
                TraceEvent::Step { pc: 0, opcode: 0xff },
                TraceEvent::SelfDestruct { contract: target, target, value: U256::from(1_u64) },
                TraceEvent::StepEnd,
                TraceEvent::CallEnd(result),
            ],
        );

        assert_ne!(revm, evm2);
    }

    fn normalize_event_pair(
        spec: SpecId,
        baseline_events: Vec<TraceEvent>,
        got_events: Vec<TraceEvent>,
    ) -> (NormalizedTxTrace, NormalizedTxTrace) {
        let mut baseline = normalize_events(baseline_events);
        let mut got = normalize_events(got_events);
        normalize_pairwise_events(&mut baseline.events, &mut got.events, spec);
        (baseline, got)
    }

    fn normalize_events(events: Vec<TraceEvent>) -> NormalizedTxTrace {
        normalize_tx_trace(&InspectorTxTrace { events, error: None })
    }

    const fn initialize(code_len: usize) -> TraceEvent {
        TraceEvent::Initialize { code_len }
    }

    fn call_trace(gas_limit: u64) -> TraceCall {
        TraceCall {
            kind: TraceCallKind::Call,
            caller: Address::ZERO,
            target: Address::ZERO,
            code_address: Address::ZERO,
            value: U256::ZERO,
            gas_limit,
            input_len: 0,
            is_static: false,
        }
    }

    fn create_trace(gas_limit: u64, input_len: usize) -> TraceCreate {
        TraceCreate {
            kind: TraceCreateKind::Create,
            caller: Address::ZERO,
            value: U256::ZERO,
            gas_limit,
            input_len,
        }
    }

    const fn trace_result(kind: TraceResultKind) -> TraceResult {
        TraceResult { kind, output_len: 0, created_address: None }
    }

    const fn trace_create_result(
        kind: TraceResultKind,
        created_address: Option<Address>,
    ) -> TraceResult {
        TraceResult { kind, output_len: 0, created_address }
    }
}
