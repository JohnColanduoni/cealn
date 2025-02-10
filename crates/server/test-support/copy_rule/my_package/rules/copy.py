from cealn.attribute import LabelAttribute
from cealn.rule import Rule


class Copy(Rule):
    src = LabelAttribute()
    dest = LabelAttribute()


assert len(Copy.attributes) == 2
assert Copy.attributes["src"] == Copy.src
assert Copy.attributes["dest"] == Copy.dest
