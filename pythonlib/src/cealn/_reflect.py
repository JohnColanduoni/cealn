import sys
import inspect


def get_caller_location() -> inspect.Traceback:
    # We want the frame for the caller of the caller of THIS function
    # The second parameter indicates we don't care abouut the context (this greatly speeds up fetching the frame)
    frame = inspect.getframeinfo(sys._getframe(2), 0)
    return frame
