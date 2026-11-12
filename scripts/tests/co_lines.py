"""Emit CPython's own line mappings for both its normal and no-column tables."""

import types

source = """
def outer(value):
    def inner():
        return value
    try:
        for i in range(10):
            if i % 2:
                value += inner()
        return [x * x for x in range(value)]
    except Exception:
        return -1
    finally:
        value = 0

async def asynchronous(iterator):
    async for item in iterator:
        yield item

class Example:
    def method(self):
        yield from range(10)
"""
# Exercise large line deltas, long columns, and long bytecode ranges.
source += "\n" * 300 + "def wide():\n    return (\n"
source += "".join(" " * 100 + f"{i},\n" for i in range(350))
source += "    )\n"
source += "def many(value):\n" + "".join(f"    value += {i}\n" for i in range(350))
source += "    return value\n"


def emit(code):
    expected = [-1] * len(code.co_code)
    for start, end, line in code.co_lines():
        expected[start:end] = [line if line is not None else -1] * (end - start)
    print(
        f"{code.co_firstlineno}|{code.co_linetable.hex()}|"
        + ",".join(map(str, expected))
    )
    for constant in code.co_consts:
        if isinstance(constant, types.CodeType):
            emit(constant)


emit(compile(source, "resolver_é_中_🐍.py", "exec"))
