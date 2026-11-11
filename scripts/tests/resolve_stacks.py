"""Stack shapes called by the Rust resolver tests with a test-only assert_stack hook."""

import sys


def direct():  # first: direct
    return assert_stack(expected)  # call: direct


def nested():  # first: nested
    def outer():  # first: outer
        def inner():  # first: inner
            return assert_stack(expected)  # call: inner

        return inner()  # call: outer

    return outer()  # call: nested


class Café:
    def café(self):  # first: café
        return self.中文()  # call: café

    def 中文(self):  # first: 中文
        return self.𐐀()  # call: 中文

    def 𐐀(self):  # first: 𐐀
        return assert_stack(expected)  # call: 𐐀


def methods():  # first: methods
    return Café().café()  # call: methods


def recursive(depth):  # first: recursive
    if depth:
        return recursive(depth - 1)  # call: recursive
    return assert_stack(expected)  # leaf: recursive


def generator_leaf():  # first: generator_leaf
    yield assert_stack(expected)  # call: generator_leaf


def generator_outer():  # first: generator_outer
    yield from generator_leaf()  # call: generator_outer


def generators():  # first: generators
    return next(generator_outer())  # call: generators


async def coroutine_leaf():  # first: coroutine_leaf
    return assert_stack(expected)  # call: coroutine_leaf


async def coroutine_outer():  # first: coroutine_outer
    return await coroutine_leaf()  # call: coroutine_outer


def coroutines():  # first: coroutines
    try:
        coroutine_outer().send(None)  # call: coroutines
    except StopIteration as done:
        return done.value


def comprehension():  # first: comprehension
    return [assert_stack(expected) for _ in range(1)][0]  # call: comprehension


def map_leaf(_):  # first: map_leaf
    return assert_stack(expected)  # call: map_leaf


def c_reentry():  # first: c_reentry
    return list(map(map_leaf, [0]))[0]  # call: c_reentry


def exceptions():  # first: exceptions
    try:
        raise ValueError("take the exception branch")
    except ValueError:
        return direct()  # call: exceptions


def cross_module():  # first: cross_module
    return foreign.leaf()  # call: cross_module


def empty_lines():  # first: empty_lines
    return empty_lines_leaf()  # call: empty_lines


def empty_lines_leaf():  # first: empty_lines_leaf
    return assert_stack(expected, 7)  # call: empty_lines_leaf


empty_lines_leaf.__code__ = empty_lines_leaf.__code__.replace(co_linetable=b"")


def unmapped_lines():  # first: unmapped_lines
    return unmapped_lines_middle()  # call: unmapped_lines


def unmapped_lines_middle():  # first: unmapped_lines_middle
    return mapped_lines_leaf()  # call: unmapped_lines_middle


def mapped_lines_leaf():  # first: mapped_lines_leaf
    return assert_stack(expected, 11)  # call: mapped_lines_leaf


# A complete mapping for only the first instruction; the active call is outside it.
unmapped_lines_middle.__code__ = unmapped_lines_middle.__code__.replace(
    co_linetable=b"\x02\x00" if sys.version_info < (3, 11) else b"\x80\x00"
)
