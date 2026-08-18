import os


def path_component(value, field):
    if not value or value in (".", "..") or "\x00" in value:
        raise ValueError(f"invalid APK {field}: {value!r}")
    if os.path.isabs(value) or os.path.sep in value:
        raise ValueError(f"invalid APK {field}: {value!r}")
    if os.path.altsep and os.path.altsep in value:
        raise ValueError(f"invalid APK {field}: {value!r}")
    return value


def package_filename(name, version):
    return f"{path_component(name, 'package name')}-{path_component(version, 'version')}.apk"


def cache_path(root, filename):
    root = os.path.realpath(root)
    target = os.path.realpath(os.path.join(root, filename))
    try:
        contained = os.path.commonpath((root, target)) == root
    except ValueError:
        contained = False
    if not contained:
        raise ValueError(f"APK cache target escapes its root: {filename!r}")
    return target
