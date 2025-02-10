use std::path::Path;

use cpython::{FromPyObject, ObjectProtocol, PyDict, PyString, Python};

use cealn_data::label;
use cealn_runtime_data::rule::{PollAnalyzeTargetIn, PrepareRuleIn, StartAnalyzeTargetIn};

use crate::{error::serializable_result, Error};

json_entry_point! {
    fn cealn_prepare_rule(python: Python, input: PrepareRuleIn) -> InvocationResult<PrepareRuleOut> {
        serializable_result(do_cealn_prepare_rule(python, input))
    }
}

json_entry_point! {
    fn cealn_start_analyze_target(python: Python, input: StartAnalyzeTargetIn) -> InvocationResult<StartAnalyzeTargetOut> {
        serializable_result(do_cealn_start_analyze_target(python, input))
    }
}

json_entry_point! {
    fn cealn_poll_analyze_target(python: Python, input: PollAnalyzeRuleIn) -> InvocationResult<PollAnalyzeTargetOut> {
        serializable_result(do_cealn_poll_analyze_target(python, input))
    }
}

fn do_cealn_prepare_rule(python: Python, input: PrepareRuleIn) -> Result<PyString, Error> {
    let workspace_name = match input.rule.source_label.root() {
        label::Root::Workspace(name) => name,
        _ => panic!("expected label with explicit workspace"),
    };
    let rule_source_workspace_relative_path = input
        .rule
        .source_label
        .to_native_relative_path()
        .expect("failed to create relative path from rule reference")
        .into_owned();
    let workspace_prefix = Path::new("/workspaces").join(workspace_name);
    let rule_source_absolute_path = workspace_prefix.join(&rule_source_workspace_relative_path);

    let start_locals = PyDict::new(python);
    start_locals.set_item(python, "rule_file", rule_source_absolute_path.to_str().unwrap())?;
    start_locals.set_item(python, "class_name", &input.rule.qualname)?;
    python.run(
        "from cealn.rule import _prepare_rule; _prepare_rule(rule_file, class_name);",
        Some(&PyDict::new(python)),
        Some(&start_locals),
    )?;

    Ok(PyString::new(python, "{}"))
}

fn do_cealn_start_analyze_target(python: Python, input: StartAnalyzeTargetIn) -> Result<PyString, Error> {
    let workspace_name = match input.target.rule.source_label.root() {
        label::Root::Workspace(name) => name,
        _ => panic!("expected label with explicit workspace"),
    };
    let rule_source_workspace_relative_path = input
        .target
        .rule
        .source_label
        .to_native_relative_path()
        .expect("failed to create relative path from rule reference")
        .into_owned();
    let workspace_prefix = Path::new("/workspaces").join(workspace_name);
    let rule_source_absolute_path = workspace_prefix.join(&rule_source_workspace_relative_path);

    let start_locals = PyDict::new(python);
    start_locals.set_item(python, "rule_file", rule_source_absolute_path.to_str().unwrap())?;
    start_locals.set_item(python, "class_name", &input.target.rule.qualname)?;
    start_locals.set_item(python, "target_name", &input.target.name)?;
    start_locals.set_item(python, "target_label", &input.target_label.as_str())?;
    let attributes_json = serde_json::to_string(&input.target.attributes_input).unwrap();
    start_locals.set_item(python, "attributes_json", &attributes_json)?;
    let build_config = serde_json::to_string(&input.build_config).unwrap();
    start_locals.set_item(python, "build_config", &build_config)?;
    python.run(
        "from cealn.rule import _start_rule; _start_rule(rule_file, class_name, target_name, target_label, attributes_json, build_config);",
        Some(&PyDict::new(python)),
        Some(&start_locals),
    )?;

    Ok(PyString::new(python, "{}"))
}

fn do_cealn_poll_analyze_target(python: Python, input: PollAnalyzeTargetIn) -> Result<PyString, Error> {
    let poll_locals = PyDict::new(python);
    let event_json = serde_json::to_string(&input.event).unwrap();
    poll_locals.set_item(python, "event_json", &event_json)?;
    python.run(
        "from cealn.rule import _poll_rule; requests_json = _poll_rule(event_json);",
        Some(&PyDict::new(python)),
        Some(&poll_locals),
    )?;

    let requests_json = poll_locals.get_item(python, "requests_json").unwrap();
    let requests_json = PyString::extract(python, &requests_json)?;

    Ok(requests_json)
}
