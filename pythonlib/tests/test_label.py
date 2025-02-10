from cealn.label import Label
from cealn._json import encode_json, decode_json

import json


def test_label_to_str():
    label = Label("@my_workspace//my_package:my_file")

    assert str(label) == "@my_workspace//my_package:my_file"


def test_label_json_encode():
    label = Label("@my_workspace//my_package:my_file")

    assert encode_json(label) == '{"$cealn_label":"@my_workspace//my_package:my_file"}'


def test_label_json_decode():
    label = Label("@my_workspace//my_package:my_file")

    assert decode_json('{"$cealn_label":"@my_workspace//my_package:my_file"}') == label
