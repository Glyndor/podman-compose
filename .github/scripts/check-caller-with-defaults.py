#!/usr/bin/env python3
"""Refuse a caller's `with:` value that differs from the reusable's declared
default for any input the reusable emits as a required check.

A pull request can edit the caller's `with:` to alter what a required job does
the same way an `if:` line can switch off a required job: the ruleset matches
on the CHECK NAME GitHub reports, not on the inputs the job received. Setting
`coverage-threshold: 76` -> 0 keeps `rust / Coverage` as the name while
lowering the gate below any failing build, and the same shape holds for
`msrv: 1.85` -> '', `extra-test-os: '["macos-latest", ...]'` -> '[]',
`doc-warnings: true` -> false, `extensions: 'rs go ... yml yaml'` -> a subset,
`working-directory: '.'` -> 'src', and any other input the ruleset depends on.

The fix is to compare each caller's `with:` value to the reusable's declared
`inputs.X.default`. A value that matches the default is invisible to the
runner (it would have been applied as the default anyway), so the ruleset
sees the same job either way. A value that DIFFERS from the default is a
pull request changing what a required job does; the step refuses it with
a message that names the file, the job, the input key, the supplied value,
and the declared default.

The reusable is the place where the value the ruleset assumes is canonicalised.
A pull request that changes a default is reviewed on the reusable's diff,
where the ruleset's check name is fixed by definition (the reusable emits
the same `name:` either way), so the change has to be deliberate and visible.

Same shape as caller-if in reusable-workflow-lint.yml: same static map of
required reusables, same direct-emitter list (which has no `with:` to
compare, so it never trips), same `reusable-*` filename filter that exempts
the reusable's own input declarations. An unknown `with:` key (the reusable
does not declare it) is silently allowed: GitHub itself ignores it, and
the ruleset does not gate on it.

Output: writes one `::error` line per violation to stdout, exits 0 on
clean and 1 on any violation. The step body in
.github/workflows/reusable-workflow-lint.yml is a thin shell-out to this
script; the extracted step body the shell test suites run is this script.

Reads: .github/workflows/*.yml, .github/workflows/*.yaml under the working
directory. Symlinks are not followed: a caller escaping the filter through
a symlinked workflow file is out of scope.
"""
import glob
import os
import sys

import yaml

# Same map of required-emitting reusables as the caller-if step in
# .github/workflows/reusable-workflow-lint.yml. A change here must move
# together with caller-if: a reusable removed from the ruleset is removed
# from both lists or the two steps drift in what they consider required.
REQUIRED_REUSABLES = {
    "reusable-rust-ci.yml": [
        "Format & lint", "Test", "Coverage", "Doc warnings",
        "MSRV", "Extra platforms",
    ],
    "reusable-dco.yml": [
        "Signed-off-by present on every commit",
    ],
    "reusable-line-limit.yml": [
        "line limit",
    ],
    "reusable-main-guard.yml": [
        "develop-only",
    ],
    "reusable-workflow-lint.yml": [
        "workflow-lint",
    ],
}


def yaml_scalar(value):
    # The reusable's `inputs.X.default` and the caller's `with.X` value
    # are both reachable as a Python value after yaml.safe_load.
    # Comparing them as `==` works for the scalars that matter here
    # (strings, numbers, booleans, and the JSON-array strings
    # cargo-cyclonedx and similar tools accept). Lists and dicts do not
    # appear as declared defaults in the required-emitting reusables
    # the lint walks; if they ever do, they hit a separate problem
    # (the default is opaque) and the assertion treats that as "literal
    # scalar expected".
    if isinstance(value, (str, int, float, bool)) or value is None:
        return value
    if isinstance(value, list):
        return value
    return repr(value)


def main():
    files = sorted(set(
        glob.glob(".github/workflows/*.yml")
        + glob.glob(".github/workflows/*.yaml")
    ))

    # First pass: read each reusable's declared `inputs.X.default`.
    # The map is `reusable-name -> {key: default}`. An unknown reusable
    # is not in scope; a caller wrapping it has no `with:` to compare.
    reusable_defaults = {}
    for path in files:
        name = os.path.basename(path)
        if not name.startswith("reusable-"):
            continue
        if name not in REQUIRED_REUSABLES:
            continue
        try:
            with open(path) as fh:
                spec = yaml.safe_load(fh)
        except yaml.YAMLError as e:
            # Warn and skip; same shape as the other lint steps in this
            # file. A misconfigured workflow is not this step's reason
            # to fail the run.
            print(f"::warning file={path}::skipped: YAML parse error: {e}")
            continue
        if not isinstance(spec, dict):
            continue
        on = spec.get("on", spec.get(True))
        if not isinstance(on, dict):
            continue
        wc = on.get("workflow_call")
        if not isinstance(wc, dict):
            continue
        inputs = wc.get("inputs")
        if not isinstance(inputs, dict):
            continue
        defaults = {}
        for key, decl in inputs.items():
            if not isinstance(decl, dict):
                continue
            defaults[key] = yaml_scalar(decl.get("default"))
        reusable_defaults[name] = defaults

    violations = []

    for path in files:
        name = os.path.basename(path)
        # A reusable's own `with:`-shaped block is its `inputs.X.default`
        # declaration, not a caller override. The reusables are filtered
        # out for the same reason as caller-if: the `if:` seam carries
        # the same shape and the same exemption.
        if name.startswith("reusable-"):
            continue
        try:
            with open(path) as fh:
                spec = yaml.safe_load(fh)
        except yaml.YAMLError as e:
            print(f"::warning file={path}::skipped: YAML parse error: {e}")
            continue
        if not isinstance(spec, dict):
            continue
        jobs = spec.get("jobs") or {}
        if not isinstance(jobs, dict):
            continue
        for job_id, job in jobs.items():
            if not isinstance(job, dict):
                continue
            uses = job.get("uses")
            if not (isinstance(uses, str) and uses.startswith("./")):
                # Direct emitters (jobs whose `name:` is the required
                # check itself) do not pass a `with:` block.
                continue
            ref = uses[2:]
            reusable_name = None
            for entry in reusable_defaults:
                if ref.endswith(entry):
                    reusable_name = entry
                    break
            if reusable_name is None:
                # Caller of a reusable that is not in the required list.
                # Out of scope; the ruleset matches the reusable's
                # emitted check name, and a non-required reusable's
                # gates are advisory, not load-bearing.
                continue
            with_inputs = job.get("with")
            if not isinstance(with_inputs, dict):
                # No `with:` block at all is the cleanest case: every
                # input takes its declared default, which is what the
                # ruleset sees.
                continue
            defaults = reusable_defaults[reusable_name]
            for input_key, supplied in with_inputs.items():
                if input_key not in defaults:
                    # The reusable declares no such input. GitHub
                    # itself ignores an undeclared `with:` key, so the
                    # lint does too.
                    continue
                if yaml_scalar(supplied) != defaults[input_key]:
                    violations.append((
                        path, job_id, reusable_name, input_key,
                        supplied, defaults[input_key],
                    ))

    if violations:
        for path, job_id, reusable_name, input_key, supplied, default in violations:
            print(
                f"::error file={path}::job `{job_id}` in {path} "
                f"calls `./{reusable_name}` with `with: "
                f"{input_key}: {supplied!r}`, which differs from "
                f"the reusable's declared default "
                f"`{default!r}`. A pull request can edit this "
                "value to alter what a required check does "
                "while the ruleset still matches the same check "
                "name. Either drop the `with:` entry to let the "
                "default apply, or update the reusable's "
                f"`inputs.{input_key}.default` so the value the "
                "ruleset assumes is the one the caller passes."
            )
        sys.exit(1)
    print(
        "caller-with-defaults: every caller of a required check "
        "carries `with:` values equal to the reusable's declared "
        "defaults."
    )


if __name__ == "__main__":
    main()
