from free_target import normalize
from method_target import Formatter


def call_free(value):
    return normalize(value)


def call_method(formatter, value):
    return formatter.normalize(value)


def call_module_qualified(helpers, value):
    return helpers.normalize(value)


def call_dynamic(formatter, value):
    dynamic = getattr(formatter, "normalize")
    return dynamic(value)


def call_dynamic_receiver(factory, value):
    return factory().normalize(value)
