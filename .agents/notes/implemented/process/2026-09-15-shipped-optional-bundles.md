# Agent Note: Ship optional bundles with the installation

Status: implemented

## Problem

A bundle installed with the application should be available to a profile without another registry download. Installation ownership must also prevent the profile manager from removing an application dependency.

## Decision

The plugin manager discovers bundles from the installation's dependencies and the profile's selected and installed packages. An installation dependency that declares `dsh.bundle.patch` and appears in no shipped profile template is `optional`, unless it protects the manager itself. It stays inactive until selected and cannot be removed from the installation. The [plugin-manager README](../../../../packages/boot/plugin-manager/README.md) owns the current API.

The installation manifest controls which bundles the product supplies. A bundle's own metadata identifies its patch but does not grant installation ownership. Packages without a bundle patch are omitted unless selected, in which case the manager reports a `not-bundle` problem.

## Alternatives considered

**A catalog of installable official bundles.** Offering registry names on demand keeps the installation smaller but requires network access when a person activates the bundle and a version pin per release.

**A flag on the bundle package.** A `dsh.bundle.optional` declaration would let any published bundle claim installation status. Deriving ownership from the installation's dependencies keeps that choice with the product.

## Consequences

Installation-provided bundles can be selected without downloading them again. The manager preserves installation ownership while allowing profile activation. It does not select companion bundles automatically; a bundle that requires another layer must document that requirement.

The [manager tests](../../../../packages/boot/plugin-manager/tests/manager.spec.ts) exercise discovery, activation, removal refusal, and omission of an unselected installation dependency without a bundle patch.
