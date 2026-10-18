"""Rewrite one boolean key inside one table of the app's config, for the e2e scenarios.

A scenario that flips `windows.show_other_desktops` hands the app the user's own
`~/.config/oh-my-tab/config.toml`. Writing the key twice into `[windows]` makes the whole file
unparseable, the app silently falls back to its defaults, and the scenario then asserts against a
state it never set. `set_bool` replaces an existing occurrence of the key however it is indented and
touches no other table; `apply` refuses to write anything the TOML parser cannot read back.

`python3 scripts/e2e/lib/config_switch.py --selftest` runs the regression cases.
"""

from __future__ import annotations

import os
import re
import sys
import tempfile

try:  # Python >= 3.11
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - the rewrite is still duplicate-safe without it
    tomllib = None

_TABLE_HEADER = re.compile(r"^\[\s*([A-Za-z0-9_.\-]+)\s*\]$")
_ARRAY_TABLE_HEADER = re.compile(r"^\[\[\s*([A-Za-z0-9_.\-]+)\s*\]\]$")


def _table_of(line):
    """The table a line opens, or None when the line is not a table header."""
    match = _TABLE_HEADER.match(line.strip())
    return match.group(1) if match else None


def _is_assignment(line, key):
    head, separator, _ = line.strip().partition("=")
    return bool(separator) and head.strip() == key


def set_bool(text, table, key, value):
    """Return `text` with `table`.`key` set to `value`, replacing any existing occurrence."""
    current = None
    kept = []
    for line in text.splitlines(keepends=True):
        opened = _table_of(line)
        if opened is not None:
            current = opened
        if current == table and _is_assignment(line, key):
            continue
        kept.append(line)

    literal = "true" if value else "false"
    updated = []
    inserted = False
    for line in kept:
        updated.append(line)
        if not inserted and _table_of(line) == table:
            updated.append(f"{key} = {literal}\n")
            inserted = True
    if not inserted:
        raise ValueError(f"the config has no [{table}] table to write {key} into")
    return "".join(updated)


def _table_header_of(line):
    """The table a line opens, for both `[table]` and `[[table]]` headers."""
    array_match = _ARRAY_TABLE_HEADER.match(line.strip())
    if array_match:
        return array_match.group(1)
    return _table_of(line)


def set_string(text, table, key, value):
    """Return `text` with `table`.`key` set to the quoted string `value`.

    Same contract as `set_bool` -- one occurrence, inside its own table, every other line untouched --
    because the caller (`panel-contrast.sh`) rewrites a key of the user's real config and must be able
    to put exactly that key back. A missing table is refused rather than invented: appending the key
    nowhere would leave the config in a state the app silently replaces with defaults.
    """
    quoted = '"' + value.replace('\\', '\\\\').replace('"', '\\"') + '"'
    current = None
    kept = []
    for line in text.splitlines(keepends=True):
        opened = _table_of(line)
        if opened is not None:
            current = opened
        if current == table and _is_assignment(line, key):
            continue
        kept.append(line)

    updated = []
    inserted = False
    for line in kept:
        updated.append(line)
        if not inserted and _table_of(line) == table:
            updated.append(f"{key} = {quoted}\n")
            inserted = True
    if not inserted:
        raise ValueError(f"the config has no [{table}] table to write {key} into")
    return "".join(updated)


def replace_key_bindings(text, items):
    """Return `text` with the whole `[key_bindings]` table replaced by `items`.

    `items` is a sequence of (action, trigger) pairs, each written enabled. Every other table is
    preserved verbatim, so a scenario that forces specific bindings does not also drop the user's
    other settings. The caller validates the result with `tomllib` before writing.
    """
    kept = []
    skipping = False
    for line in text.splitlines(keepends=True):
        header = _table_header_of(line)
        if header is not None:
            skipping = header == "key_bindings" or header.startswith("key_bindings.")
            if skipping:
                continue
        if skipping:
            continue
        kept.append(line)

    block = ["\n[key_bindings]\nenabled = true\n"]
    for action, trigger in items:
        block.append("\n[[key_bindings.items]]\n")
        block.append(f'action = "{action}"\n')
        block.append(f'trigger = "{trigger}"\n')
        block.append("enabled = true\n")
    return "".join(kept) + "".join(block)


def apply_key_bindings(path, items):
    """Rewrite only the `[key_bindings]` table; refuse to leave an unparseable config behind."""
    with open(path, encoding="utf-8") as handle:
        text = handle.read()
    updated = replace_key_bindings(text, items)
    if tomllib is not None:
        parsed = tomllib.loads(updated)
        assert parsed["key_bindings"]["enabled"] is True
        assert len(parsed["key_bindings"]["items"]) == len(items)
        for table in tomllib.loads(text):
            if table != "key_bindings":
                assert table in parsed, f"replace_key_bindings dropped [{table}]"
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(updated)
    return updated


def apply(path, table, key, value):
    """Rewrite the key in the file at `path`; never leave an unparseable config behind."""
    with open(path, encoding="utf-8") as handle:
        text = handle.read()
    updated = set_bool(text, table, key, value)
    if tomllib is not None:
        tomllib.loads(updated)
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(updated)
    return updated


def unset_key(text, table, key):
    """Return `text` with `table`.`key` removed, leaving every other line untouched.

    Used by a scenario that had to write a key the user's config did not have: restoring "absent" is
    not the same as restoring a default, and writing the default would silently add a setting the user
    never chose. The line is located the same way `set_string` locates it, so the two cannot disagree
    about which occurrence belongs to the table.
    """
    current = None
    kept = []
    for line in text.splitlines(keepends=True):
        opened = _table_of(line)
        if opened is not None:
            current = opened
        if current == table and _is_assignment(line, key):
            continue
        kept.append(line)
    return "".join(kept)


def apply_string(path, table, key, value):
    """Rewrite one string key in place; never leave an unparseable config behind."""
    with open(path, encoding="utf-8") as handle:
        text = handle.read()
    updated = set_string(text, table, key, value)
    if tomllib is not None:
        parsed = tomllib.loads(updated)
        table_values = parsed
        for part in table.split("."):
            table_values = table_values[part]
        assert table_values[key] == value
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(updated)
    return updated


def apply_unset(path, table, key):
    """Remove one key in place; never leave an unparseable config behind."""
    with open(path, encoding="utf-8") as handle:
        text = handle.read()
    updated = unset_key(text, table, key)
    if tomllib is not None:
        parsed = tomllib.loads(updated)
        table_values = parsed
        for part in table.split("."):
            table_values = table_values[part]
        assert key not in table_values
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(updated)
    return updated


def read_string(path, table, key):
    """The string value of `table`.`key`, or None when the key is ABSENT.

    A scenario that has to put a key back needs the value that was really there, read the way the app
    reads it: a table-unaware grep would happily pick up a same-named key from another table. A file
    that cannot be read raises ConfigUnreadable instead of answering "absent": the caller must not
    overwrite what it could not read.
    """
    with open(path, encoding="utf-8") as handle:
        text = handle.read()
    parsed = _parsed(text)
    values = parsed
    for part in table.split("."):
        if not isinstance(values, dict) or part not in values:
            return None
        values = values[part]
    if not isinstance(values, dict) or key not in values:
        return None
    value = values[key]
    return value if isinstance(value, str) else None


class ConfigUnreadable(Exception):
    """The config could not be parsed, so nothing may be concluded about its keys.

    Distinct from "the key is absent": a caller that treats the two alike would overwrite a value it
    never read, and put "absent" back on exit -- deleting a setting the user had.
    """


def _parsed(text):
    """The parsed document, or a raise when there is no parser to read it with.

    Returning None here would be read by every caller as "no keys", which is the failure this raises
    instead: a scenario must stop before modifying a file it cannot read, not fall back to guessing.
    """
    if tomllib is None:
        raise ConfigUnreadable("no TOML parser (tomllib) is available to read the config")
    try:
        return tomllib.loads(text)
    except tomllib.TOMLDecodeError as error:
        raise ConfigUnreadable(f"the config is not valid TOML: {error}") from error


def _selftest():
    base = "[layout]\nthumbnails_enabled = false\n\n[windows]\nenabled = true\n"
    # A missing key is added inside its table.
    out = set_bool(base, "windows", "show_other_desktops", False)
    assert out.count("show_other_desktops") == 1
    parsed = _parsed(out)
    if parsed is not None:
        assert parsed["windows"]["show_other_desktops"] is False

    # An existing key is replaced, however it is written: plain, indented, or with a tab. The
    # indented case is the counter-example that a `startswith` filter gets wrong -- it survives,
    # the insert then adds a second key, and the app reads the defaults.
    for existing in (
        "show_other_desktops = false\n",
        "  show_other_desktops = true\n",
        "\tshow_other_desktops = true\n",
        "show_other_desktops= false   # hand-edited\n",
    ):
        text = base.replace("[windows]\n", "[windows]\n" + existing)
        out = set_bool(text, "windows", "show_other_desktops", True)
        assert out.count("show_other_desktops") == 1, out
        parsed = _parsed(out)
        if parsed is not None:
            assert parsed["windows"]["show_other_desktops"] is True
            assert parsed["windows"]["enabled"] is True

    # The same key in another table is left alone.
    other = "[a]\nshow_other_desktops = false\n\n[b]\nenabled = true\n"
    out = set_bool(other, "b", "show_other_desktops", True)
    parsed = _parsed(out)
    if parsed is not None:
        assert parsed["a"]["show_other_desktops"] is False
        assert parsed["b"]["show_other_desktops"] is True

    # A missing table is refused instead of appending a key nowhere.
    try:
        set_bool("[other]\nx = 1\n", "windows", "show_other_desktops", True)
    except ValueError:
        pass
    else:
        raise AssertionError("a missing table must be refused")

    # The whole key-bindings table is replaced while every other table survives. The
    # counter-example is a scenario that adds its own items without removing the user's list: the
    # app would then see duplicate triggers and silently drop one of them.
    base = (
        '[keyboard]\nmodifier = "command"\n\n'
        "[key_bindings]\nenabled = false\n\n"
        '[[key_bindings.items]]\naction = "quick.open_finder"\ntrigger = "option+e"\nenabled = true\n\n'
        "[windows]\nenabled = true\n"
    )
    out = replace_key_bindings(
        base,
        [
            ("quick.open_finder", "control+option+e"),
            ("quick.open_finder_small", "control+option+shift+e"),
        ],
    )
    assert out.count("[[key_bindings.items]]") == 2, out
    assert 'trigger = "option+e"' not in out, out
    parsed = _parsed(out)
    if parsed is not None:
        assert parsed["keyboard"]["modifier"] == "command"
        assert parsed["windows"]["enabled"] is True
        assert parsed["key_bindings"]["enabled"] is True
        assert [item["action"] for item in parsed["key_bindings"]["items"]] == [
            "quick.open_finder",
            "quick.open_finder_small",
        ]

    # --- string keys ---------------------------------------------------------------------------
    # panel-contrast rewrites the user's `appearance.theme` and puts it back. It has to survive the
    # three shapes a hand-edited config holds it in, and it must not touch any other key.
    appearance = (
        "[appearance]\ntheme = \"auto\"\npanel_material = \"frost\"\n\n"
        "[clipboard]\nenabled = false\n"
    )
    out = set_string(appearance, "appearance", "theme", "dark")
    assert out.count("theme = ") == 1, out
    parsed = _parsed(out)
    if parsed is not None:
        assert parsed["appearance"]["theme"] == "dark"
        assert parsed["appearance"]["panel_material"] == "frost"
        assert parsed["clipboard"]["enabled"] is False

    # An indented key is replaced, not duplicated -- the same counter-example set_bool carries.
    out = set_string("[appearance]\n  theme = \"auto\"\n", "appearance", "theme", "light")
    assert out.count("theme") == 1, out
    parsed = _parsed(out)
    if parsed is not None:
        assert parsed["appearance"]["theme"] == "light"

    # A missing key is added inside its table, not at the end of the file.
    out = set_string("[a]\nx = 1\n\n[b]\ny = 2\n", "a", "theme", "dark")
    assert out.index("theme") < out.index("[b]"), out

    # A missing table is refused instead of appending nowhere.
    try:
        set_string("[other]\nx = 1\n", "appearance", "theme", "dark")
    except ValueError:
        pass
    else:
        raise AssertionError("a missing table must be refused")

    # Removing a key restores "absent", which is what a config the scenario found without the key must
    # get back; the other keys of the table survive.
    out = unset_key('[appearance]\ntheme = "dark"\npanel_material = "frost"\n', "appearance", "theme")
    assert "theme" not in out, out
    assert 'panel_material = "frost"' in out
    parsed = _parsed(out)
    if parsed is not None:
        assert "theme" not in parsed["appearance"]

    # A value that needs escaping is written as a valid TOML string rather than breaking the file.
    out = set_string("[appearance]\n", "appearance", "theme", 'da"rk')
    parsed = _parsed(out)
    if parsed is not None:
        assert parsed["appearance"]["theme"] == 'da"rk', out

    # Reading back what a restore has to put in place: a same-named key in another table must not be
    # mistaken for it, and an absent key must be distinguishable from an empty string.
    two_tables = '[appearance]\ntheme = "dark"\n\n[colors.dark]\ntheme = "not-the-setting"\n'
    with tempfile.NamedTemporaryFile("w", suffix=".toml", delete=False) as handle:
        handle.write(two_tables)
        scratch = handle.name
    try:
        assert read_string(scratch, "appearance", "theme") == "dark", read_string(scratch, "appearance", "theme")
        assert read_string(scratch, "appearance", "absent") is None
    finally:
        os.unlink(scratch)

    # An unreadable document is not "the key is absent": a caller that confused them would delete a
    # setting it never read. Both shapes are refused with the dedicated exception.
    with tempfile.NamedTemporaryFile("w", suffix=".toml", delete=False) as handle:
        handle.write("[appearance\ntheme = \"dark\"\n")
        broken = handle.name
    try:
        try:
            read_string(broken, "appearance", "theme")
        except ConfigUnreadable:
            pass
        else:
            raise AssertionError("an unparseable config must not be read as 'no such key'")
    finally:
        os.unlink(broken)

    print("config_switch selftest: ok")


if __name__ == "__main__":
    if sys.argv[1:2] == ["--selftest"]:
        _selftest()
    elif len(sys.argv) == 6 and sys.argv[1] == "--set":
        _, _, path, table, key, literal = sys.argv
        if literal not in ("true", "false"):
            raise SystemExit("the value must be true or false")
        apply(path, table, key, literal == "true")
    elif len(sys.argv) == 6 and sys.argv[1] == "--set-string":
        _, _, path, table, key, value = sys.argv
        apply_string(path, table, key, value)
    elif len(sys.argv) == 5 and sys.argv[1] == "--get-string":
        _, _, path, table, key = sys.argv
        try:
            value = read_string(path, table, key)
        except ConfigUnreadable as error:
            print(f"config_switch: {error}", file=sys.stderr)
            raise SystemExit(4)
        if value is None:
            raise SystemExit(3)
        print(value)
    elif len(sys.argv) == 5 and sys.argv[1] == "--unset":
        _, _, path, table, key = sys.argv
        apply_unset(path, table, key)
    elif len(sys.argv) >= 4 and sys.argv[1] == "--set-bindings":
        path = sys.argv[2]
        items = []
        for spec in sys.argv[3:]:
            action, separator, trigger = spec.partition("=")
            if not separator or not action or not trigger:
                raise SystemExit(f"bad binding spec: {spec!r}")
            items.append((action, trigger))
        apply_key_bindings(path, items)
    else:
        raise SystemExit(
            "usage: config_switch.py --selftest | --set <path> <table> <key> <true|false> |"
            " --set-string <path> <table> <key> <value> | --unset <path> <table> <key> |"
            " --get-string <path> <table> <key> |"
            " --set-bindings <path> <action=trigger>..."
        )
