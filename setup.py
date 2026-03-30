#!/usr/bin/env python3
"""
Hermes Agent setup.py

This file exists for backward compatibility and for workflows that require
`python setup.py develop` or `python setup.py install`. The canonical
dependency and project metadata lives in pyproject.toml — this file delegates
to it where possible.

Usage:
    python setup.py develop      (install in dev mode)
    python setup.py build         (build wheel/sdist)
    pip install -e ".[all]"       (preferred install method)
"""

from setuptools import setup

if __name__ == "__main__":
    # Defer all package discovery and metadata to pyproject.toml.
    # setup() here only needs to exist for compatibility; pyproject.toml
    # drives the actual build.
    setup()
