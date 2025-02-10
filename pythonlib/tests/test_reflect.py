from cealn._reflect import get_caller_location


def test_get_caller_location():
    def intermediate_function():
        return get_caller_location()

    location = intermediate_function()

    assert location.filename.endswith("test_reflect.py")
    assert location.function == "test_get_caller_location"
