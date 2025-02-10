from fnmatch import fnmatch
import re
from typing import List, Union


class GlobSet:
    patterns: List[Union[str, re.Pattern]]

    def __init__(self, *args):
        self.patterns = list(args)

    def match(self, item):
        for pattern in self.patterns:
            if isinstance(pattern, str):
                if fnmatch(item, pattern):
                    return True
            elif isinstance(pattern, re.Pattern):
                if pattern.match(item):
                    return True
        return False

    def to_json(self):
        return {_JSON_GLOBSET_SENTINEL: self.patterns}

    @classmethod
    def from_json(cls, raw):
        return GlobSet(*raw[_JSON_GLOBSET_SENTINEL])


_JSON_GLOBSET_SENTINEL = "$cealn_globset"
