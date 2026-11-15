"""Builds the platform from modules/*.mod with the settings in build.cfg.

The build runs in phases: compile, link, package. A phase that rejects
anything stops the build after it, so later phases only report once the
earlier ones are clean. Exits 0 when the package is built, else 1.
"""

import configparser
import hashlib
import os
import sys

# Each library version: the API levels it supports, and the symbols it
# exports at an API level.
LIBRARIES = {
    "store_v1": (2, 5, lambda api: ("get", "put")),
    "store_v2": (3, 9, lambda api: ("get", "put", "scan") if api < 6 else ("put", "scan")),
    "store_v3": (6, 9, lambda api: ("get", "put", "scan")),
    "ledger_v1": (2, 9, lambda api: ("list", "read")),
    "ledger_v2": (4, 9, lambda api: ("list", "post_entry", "read")),
    "cache_v1": (2, 9, lambda api: ("fill", "hit")),
    "cache_v2": (7, 9, lambda api: ("evict", "fill", "hit")),
}
KNOWN_FEATURES = ("cache", "http", "json", "metrics", "tls")
DEPRECATED = {"store.get": "store.scan"}


def read_config(path):
    parser = configparser.ConfigParser()
    with open(path, encoding="utf-8") as handle:
        parser.read_file(handle)
    problems = []
    api_level = parser.getint("toolchain", "api_level")
    enabled = [name.strip() for name in parser.get("features", "enabled").split(",") if name.strip()]
    for name in enabled:
        if name not in KNOWN_FEATURES:
            problems.append(f"build.cfg: there is no feature {name!r}")
    aliases = dict(parser.items("aliases"))
    for short, library in sorted(aliases.items()):
        if library not in LIBRARIES:
            problems.append(f"build.cfg: alias {short} names no known library ({library!r})")
            continue
        low, high, _ = LIBRARIES[library]
        if not low <= api_level <= high:
            problems.append(f"build.cfg: {library} supports api levels {low}..{high}, not {api_level}")
    return api_level, enabled, aliases, problems


def read_module(path):
    fields = {}
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            key, _, value = line.partition("=")
            fields[key.strip()] = value.strip()
    low, high = (int(part) for part in fields["api"].split(".."))
    split = lambda text: [item.strip() for item in text.split(",") if item.strip()]  # noqa: E731
    return {
        "name": fields["name"],
        "group": fields["group"],
        "api": (low, high),
        "features": split(fields["features"]),
        "uses": split(fields["uses"]),
        "units": int(fields["units"]),
    }


def lint_notes(module):
    return (module["units"] * 7 + len(module["uses"])) % 4


def compile_phase(modules, texts, api_level, enabled, out):
    rejected = 0
    warnings = 0
    for number, module in enumerate(modules, 1):
        name = module["name"]
        low, high = module["api"]
        reasons = []
        if not low <= api_level <= high:
            reasons.append(f"api level {api_level} is outside the module's range {low}..{high} (build.cfg [toolchain] api_level)")
        for feature in module["features"]:
            if feature not in enabled:
                reasons.append(f"needs the feature {feature}, which build.cfg [features] enabled leaves out")
        status = "REJECTED" if reasons else "ok"
        out(f"[{number:4d}/{len(modules)}] {name}  {module['group']:<10} api {low}..{high}  features {','.join(module['features'])}  {status}")
        out(f"    lint: 0 errors, {lint_notes(module)} notes")
        for use in module["uses"]:
            if use in DEPRECATED:
                out(f"    note: {use} is deprecated; prefer {DEPRECATED[use]}")
                warnings += 1
        for reason in reasons:
            out(f"    {reason}")
        digest = hashlib.sha256(texts[name].encode("utf-8")).hexdigest()[:16]
        out(f"    digest {digest}, {module['units']} units, {module['units'] * 61} bytes")
        rejected += bool(reasons)
    return rejected, warnings


def link_phase(modules, api_level, aliases, out):
    unresolved = 0
    for number, module in enumerate(modules, 1):
        resolved = []
        missing = []
        for use in module["uses"]:
            short, _, symbol = use.partition(".")
            library = aliases.get(short)
            if library is None:
                missing.append(f"{use} unresolved: build.cfg [aliases] has no {short}")
                continue
            exported = LIBRARIES[library][2](api_level)
            if symbol in exported:
                resolved.append(f"{use} -> {library}.{symbol}")
                continue
            others = [
                f"{other} exports {', '.join(LIBRARIES[other][2](api_level))}"
                for other in sorted(LIBRARIES)
                if other.startswith(short + "_") and other != library
                and LIBRARIES[other][0] <= api_level <= LIBRARIES[other][1]
            ]
            missing.append(
                f"{use} unresolved: {short} = {library}, which at api {api_level} exports "
                f"{', '.join(exported)}; " + "; ".join(others)
            )
        out(f"  [{number:4d}/{len(modules)}] {module['name']}: resolved {len(resolved)}/{len(module['uses'])}")
        for line in resolved:
            out(f"      {line}")
        for line in missing:
            out(f"      {line}")
        unresolved += len(missing)
    return unresolved


def dependency_report(modules, out):
    out("== dependency report")
    for module in modules:
        out(f"  {module['name']} ({module['group']}), {module['units']} units")
        libraries = sorted({use.partition('.')[0] for use in module["uses"]})
        out(f"    libraries: {', '.join(libraries)}")
        out(f"    features: {', '.join(module['features'])}")
        out(f"    lint: 0 errors, {lint_notes(module)} notes")


def main():
    lines = []
    out = lines.append
    api_level, enabled, aliases, problems = read_config("build.cfg")
    names = sorted(name for name in os.listdir("modules") if name.endswith(".mod"))
    texts = {}
    modules = []
    for file_name in names:
        with open(os.path.join("modules", file_name), encoding="utf-8") as handle:
            text = handle.read()
        module = read_module(os.path.join("modules", file_name))
        texts[module["name"]] = text
        modules.append(module)
    out(f"build: {len(modules)} modules, api level {api_level}, features {', '.join(enabled)}")
    out("build: aliases " + ", ".join(f"{key} = {aliases[key]}" for key in sorted(aliases)))
    for problem in problems:
        out(problem)
    used = {feature for module in modules for feature in module["features"]}
    for feature in enabled:
        if feature not in used:
            problems.append(f"build.cfg enables the feature {feature}, which no module uses")
            out(problems[-1])
    if problems:
        out(f"build stopped: {len(problems)} problems in build.cfg")
        sys.stdout.write("\n".join(lines) + "\n")
        return 1
    out("== compile")
    rejected, warnings = compile_phase(modules, texts, api_level, enabled, out)
    if rejected:
        dependency_report(modules, out)
        out(f"build stopped in compile: {rejected} modules rejected, {warnings} notes")
        sys.stdout.write("\n".join(lines) + "\n")
        return 1
    out("== link")
    unresolved = link_phase(modules, api_level, aliases, out)
    if unresolved:
        dependency_report(modules, out)
        out(f"build stopped in link: {unresolved} references unresolved")
        sys.stdout.write("\n".join(lines) + "\n")
        return 1
    out("== package")
    for group in sorted({module["group"] for module in modules}):
        members = [module for module in modules if module["group"] == group]
        out(f"  {group}: {len(members)} modules, {sum(m['units'] for m in members)} units")
    out(f"build succeeded: {len(modules)} modules")
    sys.stdout.write("\n".join(lines) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
