use std::path::Path;

use cpython::{FromPyObject, PyDict, PyString, Python};

use cealn_data::label;
use cealn_runtime_data::package_load::LoadPackageIn;

use crate::{error::serializable_result, python::run_python_file, Error};

json_entry_point! {
    fn cealn_load_package(python: Python, input: LoadPackageIn) -> InvocationResult<LoadPackageOut> {
        serializable_result(do_cealn_load_package(python, input))
    }
}

fn do_cealn_load_package(python: Python, input: LoadPackageIn) -> Result<PyString, Error> {
    // Flag this as a workspace invocation
    let set_package_locals = PyDict::new(python);
    set_package_locals.set_item(python, "label", input.package.as_str())?;
    python
        .run(
            "from cealn.package import _set_is_package; from cealn.label import Label; _set_is_package(Label(label));",
            Some(&PyDict::new(python)),
            Some(&set_package_locals),
        )
        .expect("failed to initialize package mode");

    // Prepare main module with prelude, so the imports are already there when the user-supplied script runs
    let globals = PyDict::new(python);
    python
        .run("from cealn.preludes.package import *", Some(&globals), None)
        .expect("failed to prepare package prelude");

    let workspace_name = match input.package.root() {
        label::Root::Workspace(name) => name,
        _ => panic!("expected label with explicit workspace"),
    };
    let mut build_file_workspace_relative_path = input
        .package
        .to_native_relative_path()
        .expect("failed to create relative path from package label")
        .into_owned();
    build_file_workspace_relative_path.push("build.cealn");
    let workspace_prefix = Path::new("/workspaces").join(workspace_name);
    let build_file_absolute_path = workspace_prefix.join(&build_file_workspace_relative_path);

    run_python_file(
        python,
        &build_file_absolute_path,
        &build_file_workspace_relative_path,
        &workspace_prefix,
        &format!("workspaces.{}.", workspace_name.replace(".", "_")),
        Some(&globals),
    )?;

    let configured_info_globals = PyDict::new(python);
    python.run(
        "import cealn.package; configured_info = cealn.package._get_configured_info()",
        Some(&configured_info_globals),
        None,
    )?;
    let configured_info_json = configured_info_globals
        .get_item(python, "configured_info")
        .expect("missing fetched configured info");

    let configured_info_json = PyString::extract(python, &configured_info_json)?;

    Ok(configured_info_json)
}
