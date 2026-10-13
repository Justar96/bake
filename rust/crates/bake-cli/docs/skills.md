# Skills

A skill is a Markdown file of instructions for one kind of task, in the Agent Skills format. bake-rs lists each skill's name, description, and file in the system prompt, and the model reads a skill's file when a task matches its description.

## Where they are found

bake-rs reads skills from these places, in this order, and keeps the first skill of each name:

1. `.agents/skills` in the session's working directory and in each parent up to the root of its git repository, or up to the filesystem root outside a repository, nearest first;
2. `~/.agents/skills` in your home directory;
3. each `--skill <path>`, a skill file or a directory, in the order given.

In a skills directory, each directory that holds a `SKILL.md` is one skill, and the search does not go further into it. Other `.md` files are skills too when their frontmatter has a description: in a `.agents/skills` directory those in its subdirectories, and in a `--skill` directory those directly in it. Hidden entries and `node_modules` are skipped, and `.gitignore`, `.ignore`, and `.fdignore` files inside a skills directory apply below them.

`-ns`, `--no-skills` skips the `.agents/skills` directories; skills given with `--skill` still load.

## The skill file

A skill file starts with YAML frontmatter:

```markdown
---
name: release-notes
description: Write release notes from the commits since the last tag.
---

Read the changelog first, then …
```

- `description` is required; a file without one is not loaded.
- `name` defaults to the name of the directory holding the file. The format allows lowercase letters, digits, and single hyphens, up to 64 characters.
- `disable-model-invocation: true` leaves the skill out of the system prompt, so it runs only when a prompt asks for it by name.

A relative path in a skill file is taken from the skill's directory. A skill file larger than 1 MiB is skipped. bake-rs does not report skills it skips in print mode, so check a skill's frontmatter when it does not appear.

## Running a skill by name

A prompt that starts with `/skill:<name>` sends that skill's instructions, without the frontmatter, followed by the rest of the prompt:

```sh
bake-rs -p "/skill:release-notes for v0.4.0"
```

A name that matches no loaded skill sends the prompt as written.
