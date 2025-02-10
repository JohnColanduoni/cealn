use std::path::Path;

use cpython::{FromPyObject, PyDict, PyString, Python};

use crate::{error::serializable_result, python::run_python_file, Error};

json_entry_point! {
    fn cealn_load_root_workspace(python: Python) -> InvocationResult<LoadRootWorkspaceOut> {
        serializable_result(do_cealn_load_root_workspace(python))
    }
}

fn do_cealn_load_root_workspace(python: Python) -> Result<PyString, Error> {
    // Flag this as a workspace invocation
    python.run(
        "from cealn.workspace import _set_is_workspace; _set_is_workspace();",
        Some(&PyDict::new(python)),
        None,
    )?;

    // Prepare main module with prelude, so the imports are already there when the user-supplied script runs
    let globals = PyDict::new(python);
    python
        .run("from cealn.preludes.workspace import *", Some(&globals), None)
        .expect("failed to prepare workspace prelude");

    run_python_file(
        python,
        Path::new("/workspace/workspace.cealn"),
        Path::new("workspace.cealn"),
        Path::new("/workspace"),
        "root_workspace.",
        Some(&globals),
    )?;

    let configured_info_globals = PyDict::new(python);
    python.run(
        "import cealn.workspace; configured_info = cealn.workspace._get_configured_info()",
        Some(&configured_info_globals),
        None,
    )?;
    let configured_info_json = configured_info_globals
        .get_item(python, "configured_info")
        .expect("missing fetched configured info");

    let configured_info_json = PyString::extract(python, &configured_info_json)?;

    Ok(configured_info_json)
}
