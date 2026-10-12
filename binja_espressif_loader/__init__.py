"""Python loader for binja-espressif's native plugin.

Binary Ninja's extension manager installs only Python plugins, from the
repository's source at a release tag. This package installs the matching
prebuilt native library from that release into the user plugin directory,
with the user's consent and after checking it against manifest.json, the
SHA-256 hashes committed with the release. See core.py (logic, no Binary
Ninja imports) and plugin.py (Binary Ninja integration).
"""
