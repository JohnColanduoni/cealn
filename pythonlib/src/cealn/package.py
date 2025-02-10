from ._json import encode_json

_IS_PACKAGE = False
_CONFIGURED_INFO = None


def _set_is_package(label):
    global _IS_PACKAGE
    global _CONFIGURED_INFO
    _IS_PACKAGE = True
    _CONFIGURED_INFO = dict(package=dict(label=label, targets=[]))


def _add_rule_invocation_to_package(invocation):
    global _IS_PACKAGE
    global _CONFIGURED_INFO
    if not _IS_PACKAGE:
        return
    _CONFIGURED_INFO["package"]["targets"].append(invocation.to_json())


def _get_package_optional():
    global _IS_PACKAGE
    global _CONFIGURED_INFO
    if not _IS_PACKAGE:
        return None
    return _CONFIGURED_INFO["package"]["label"]


def _get_configured_info():
    global _CONFIGURED_INFO

    import json

    if _CONFIGURED_INFO is None:
        raise RuntimeError("runtime should have called package setup")

    return encode_json(_CONFIGURED_INFO)
