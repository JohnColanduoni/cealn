import pytest

from cealn.rule import Rule


class SimpleRule(Rule):
    pass


def test_rule_requires_name():
    with pytest.raises(TypeError) as excinfo:
        instance = SimpleRule()

    assert '"name"' in str(excinfo.value)


def test_rule_invocation_location():
    instance = SimpleRule(name="whatever")

    assert instance.instantiation_location.filename.endswith("test_rule.py")
    assert instance.instantiation_location.function == "test_rule_invocation_location"
