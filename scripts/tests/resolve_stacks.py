"""Stack shapes called by the Rust resolver tests with a test-only assert_stack hook."""


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
