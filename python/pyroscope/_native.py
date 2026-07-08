
__all__ = ['lib', 'ffi']

from importlib.machinery import EXTENSION_SUFFIXES
import os
from ._cffi import ffi

extension_dir = os.path.join(os.path.dirname(__file__), '../pyroscope_python_extension')
for suffix in EXTENSION_SUFFIXES:
    path = os.path.join(extension_dir, 'pyroscope_python_extension' + suffix)
    if os.path.exists(path):
        lib = ffi.dlopen(path)
        break
else:
    raise ImportError(f'Could not find native pyroscope extension in {extension_dir}')

del EXTENSION_SUFFIXES, extension_dir, os, path, suffix
