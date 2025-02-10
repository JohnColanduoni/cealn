from importlib.abc import MetaPathFinder
from importlib.machinery import ModuleSpec
from importlib.util import spec_from_file_location
import sys


class WorkspaceFinder(MetaPathFinder):
    """
    Allows importing Python files relative to the workspace by prefixing them with `workspace.`
    """

    def find_spec(self, fullname: str, path, target=None):
        if fullname == "root_workspace":
            # FIXME: Root workspace imports are a hack we should get rid of. It makes it easy to write code that works
            # when a particular workspace is the root workspace and then breaks when it is included in another workspace.
            spec = ModuleSpec("root_workspace", None, origin=None)
            spec.submodule_search_locations = ["/workspace"]
            return spec
        elif fullname == "workspaces":
            spec = ModuleSpec("workspaces", None, origin=None)
            spec.submodule_search_locations = ["/workspaces"]
            return spec
        elif fullname.startswith("workspaces."):
            workspace_name = fullname[len("workspaces.") :]
            if "." in workspace_name:
                # Should be handled by parent's module spec
                return None
            spec = ModuleSpec(fullname, None, origin=None)
            # FIXME: What happens to workspace names with literal underscores?
            workspace_real_name = workspace_name.replace("_", ".")
            spec.submodule_search_locations = [f"/workspaces/{workspace_real_name}"]
            return spec


sys.meta_path.insert(0, WorkspaceFinder())
