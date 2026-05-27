#!/bin/sh
set -e

export HOME=/root
export USER=root
export SHELL=/bin/sh
export TERM=xterm-256color
export PATH=/usr/local/sbin:/usr/local/bin:/usr/bin:/bin:/sbin:/root/.local/bin
export PIP_BREAK_SYSTEM_PACKAGES=1

STAGE=0
fail() { echo "STARRY_PIP_STAGE_${STAGE}_FAILED: $1"; exit 1; }
next() { STAGE=$((STAGE+1)); echo "STARRY_PIP_STAGE_${STAGE}: $1 OK"; }

# ---------- Stage 1: Basic sanity ----------
python3 --version || fail "python3 --version"
pip3 --version || fail "pip3 --version"
pip3 --help > /dev/null || fail "pip3 --help"
next "basic-sanity"

# ---------- Stage 2: Create venv ----------
python3 -m venv /root/test-venv || fail "venv create"
test -d /root/test-venv/bin || fail "venv bin dir missing"
test -f /root/test-venv/bin/activate || fail "venv activate missing"
test -f /root/test-venv/bin/pip || fail "venv pip missing"
. /root/test-venv/bin/activate || fail "venv activate"
next "venv-create"

# ---------- Stage 3: pip inside venv ----------
pip --version || fail "pip --version in venv"
python -c "import sys; assert hasattr(sys, 'prefix') and hasattr(sys, 'base_prefix')" || fail "sys.prefix check"
python -c "import sys; print('real_prefix' if hasattr(sys, 'real_prefix') else 'base_prefix:', sys.base_prefix)" || fail "venv detection"
next "venv-pip"

# ---------- Stage 4: pip list (empty venv) ----------
LIST_OUT="$(pip list 2>&1)"
echo "$LIST_OUT" | grep -iE "^pip\s" || fail "pip list missing pip"
next "pip-list-empty"

# ---------- Stage 5: pip install a small package ----------
pip install --no-cache-dir pyfiglet || fail "pip install pyfiglet"
python -c "import pyfiglet; print(pyfiglet.figlet_format('OK'))" || fail "import pyfiglet"
next "pip-install"

# ---------- Stage 6: pip show ----------
SHOW_OUT="$(pip show pyfiglet 2>&1)"
echo "$SHOW_OUT" | grep -i "^Name:.*pyfiglet" || fail "pip show name"
echo "$SHOW_OUT" | grep -i "^Version:" || fail "pip show version"
echo "$SHOW_OUT" | grep -i "^Location:" || fail "pip show location"
next "pip-show"

# ---------- Stage 7: pip list (after install) ----------
pip list 2>&1 | grep -iE "^pyfiglet\s" || fail "pip list missing pyfiglet after install"
next "pip-list-after-install"

# ---------- Stage 8: pip install --dry-run ----------
pip install --dry-run --no-cache-dir requests 2>&1 | grep -i "Would install" || fail "pip install --dry-run"
next "pip-dry-run"

# ---------- Stage 9: pip freeze ----------
FREEZE_OUT="$(pip freeze 2>&1)"
echo "$FREEZE_OUT" | grep -iE "^pyfiglet==" || fail "pip freeze missing pyfiglet"
next "pip-freeze"

# ---------- Stage 10: pip freeze > requirements.txt + pip install -r ----------
pip freeze > /root/requirements.txt || fail "pip freeze > requirements.txt"
pip install --no-cache-dir -r /root/requirements.txt || fail "pip install -r requirements.txt"
next "pip-requirements"

# ---------- Stage 11: pip install --upgrade ----------
OLD_VER="$(pip show pyfiglet | grep '^Version:' | awk '{print $2}')"
pip install --no-cache-dir --upgrade pyfiglet || fail "pip install --upgrade pyfiglet"
next "pip-upgrade"

# ---------- Stage 12: pip install a second package ----------
pip install --no-cache-dir six || fail "pip install six"
python -c "import six; print('six version:', six.__version__)" || fail "import six"
next "pip-install-second"

# ---------- Stage 13: pip install multiple packages ----------
pip install --no-cache-dir chardet idna || fail "pip install chardet idna"
python -c "import chardet; import idna; print('multi-install OK')" || fail "import multi"
next "pip-install-multi"

# ---------- Stage 14: pip install from local directory ----------
mkdir -p /tmp/localpkg
cat > /tmp/localpkg/setup.py << 'PYEOF'
from setuptools import setup
setup(
    name="localpkg",
    version="0.1.0",
    py_modules=["localmod"],
)
PYEOF
cat > /tmp/localpkg/localmod.py << 'PYEOF'
HELLO = "hello from localpkg"
PYEOF
pip install --no-cache-dir /tmp/localpkg || fail "pip install local dir"
python -c "import localmod; assert localmod.HELLO == 'hello from localpkg'" || fail "import localpkg"
next "pip-install-local-dir"

# ---------- Stage 15: pip install local wheel ----------
mkdir -p /tmp/wheels
pip wheel --no-cache-dir --no-deps -w /tmp/wheels markupsafe || fail "pip wheel markupsafe"
WHEEL_FILE="$(ls /tmp/wheels/markupsafe-*.whl | head -1)"
test -f "$WHEEL_FILE" || fail "wheel file not found"
pip install --no-cache-dir "$WHEEL_FILE" || fail "pip install local wheel"
python -c "import markupsafe; print('markupsafe OK')" || fail "import markupsafe"
next "pip-install-local-wheel"

# ---------- Stage 16: pip download ----------
mkdir -p /tmp/downloads
pip download --no-cache-dir -d /tmp/downloads toml || fail "pip download toml"
test "$(ls /tmp/downloads/*.whl 2>/dev/null | wc -l)" -ge 1 || fail "no wheel downloaded"
next "pip-download"

# ---------- Stage 17: pip cache ----------
CACHE_DIR="$(pip cache dir 2>&1)" || fail "pip cache dir"
echo "cache dir: $CACHE_DIR"
pip cache info 2>&1 || fail "pip cache info"
pip install --no-cache-dir --force-reinstall --cache-dir=/tmp/pip-cache-dir pyfiglet 2>&1 || fail "pip install with custom cache dir"
test -d /tmp/pip-cache-dir || fail "custom cache dir not created"
pip cache purge 2>&1 || true
next "pip-cache"

# ---------- Stage 18: pip check ----------
pip check || fail "pip check"
next "pip-check"

# ---------- Stage 19: pip uninstall ----------
pip uninstall -y six || fail "pip uninstall six"
python -c "import six" 2>/dev/null && fail "six still importable after uninstall" || true
! pip show six >/dev/null 2>&1 || fail "pip show six still works after uninstall"
next "pip-uninstall"

# ---------- Stage 20: pip install after uninstall ----------
pip install --no-cache-dir six || fail "pip install six again"
python -c "import six" || fail "import six after reinstall"
next "pip-reinstall"

# ---------- Stage 21: editable install (pip install -e) ----------
mkdir -p /tmp/mypackage
cat > /tmp/mypackage/setup.py << 'PYEOF'
from setuptools import setup
setup(
    name="mypackage",
    version="0.1.0",
    py_modules=["mymod"],
)
PYEOF
cat > /tmp/mypackage/mymod.py << 'PYEOF'
def hello():
    return "hello from mypackage"
PYEOF
pip install --no-cache-dir -e /tmp/mypackage || fail "pip install -e"
python -c "import mymod; assert mymod.hello() == 'hello from mypackage'" || fail "editable import"
next "pip-editable"

# ---------- Stage 22: pip list with format ----------
pip list --format=json 2>&1 | python3 -c "import sys,json; d=json.load(sys.stdin); assert isinstance(d,list)" || fail "pip list --format=json"
pip list --format=columns 2>&1 | head -5 || fail "pip list --format=columns"
pip list --outdated 2>&1 || fail "pip list --outdated"
next "pip-list-format"

# ---------- Stage 23: pip config ----------
pip config list 2>&1 || fail "pip config list"
next "pip-config"

# ---------- Stage 24: pip debug ----------
pip debug 2>&1 | head -20 || fail "pip debug"
next "pip-debug"

# ---------- Stage 25: venv cleanup ----------
deactivate 2>/dev/null || true
rm -rf /root/test-venv || fail "rm venv"
test ! -d /root/test-venv || fail "venv dir still exists"
rm -f /root/requirements.txt
next "venv-cleanup"

echo ""
echo "STARRY_PIP_TESTS_PASSED"
