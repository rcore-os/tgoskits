---
name: markdown-doc-writer
description: Write, rewrite, review, or normalize Markdown technical documentation with strict structure rules. Use when Codex is asked to create or edit .md docs, especially project docs that must combine functional explanation with key source-code anchors such as data structures, functions, state machines, APIs, parameters, and Mermaid diagrams.
---

# Markdown Doc Writer

## Writing Workflow

Use this skill whenever producing Markdown documentation, whether creating new files or rewriting existing docs. Keep the document useful for engineers and product readers at the same time: explain the feature or process first, then tie that explanation to the key code objects needed for maintenance.

### Build the Outline First

Start from the domain shape, not from source-file order. Identify the real product or technical concepts, then map each concept to source-code anchors such as functions, structs, enums, constants, tables, API routes, configuration fields, or files.

Ensure the document has one meaningful H1 title, then structure the body with explicit numeric prefixes in the heading text. Body headings must not stay flat at only one level; use at least two body levels, such as `## 1. 构建流程` with `### 1.1 配置选择` and `### 1.2 内核构建` beneath it, followed by `## 2. 运行验证` with `### 2.1 QEMU 启动` and `### 2.2 板卡启动`. Choose the number of child headings from the actual domain complexity, not from a fixed template: complex feature areas should usually split into 3, 4, or more sibling sections when each section carries distinct behavior, while simple areas may keep 2 child headings. Use at most three body heading levels after the H1 title, and avoid decorative sections such as "阅读导航", "使用说明", "背景说明", or other filler that does not carry domain content.

### Write Sections as Explanation Blocks

After every heading, write a real paragraph before any diagram, table, list, or code block. The paragraph must be long enough to explain the purpose, behavior, and maintenance relevance of the section; avoid one-sentence or dozens-of-characters fragments.

Use this repeatable pattern inside every section:

1. Explanatory paragraph that combines functional behavior with key code anchors.
2. Optional Mermaid diagram, table, ordered list, unordered list, or code snippet.
3. Follow-up paragraph when the block needs interpretation, boundary conditions, or implementation notes.
4. Repeat the same paragraph-plus-block pattern as needed.

Never place two block elements directly adjacent to each other. Insert an explanatory paragraph between any two diagrams, tables, lists, or code snippets.

## Content Requirements

Markdown docs produced with this skill must be technically grounded. A section that describes behavior should normally name the key data structure, function, enum, constant, or module that implements or stores that behavior, but the document must not devolve into line-by-line source commentary.

### Code Anchors

Use inline code for names such as `cargo starry qemu`, `ArgsDefconfig`, `write_defconfig()`, `PlatformConfig`, or `vm_configs`. Use code blocks only when a short snippet is necessary to show an exact call sequence, data layout, or conditional rule.

Prefer tables for key code anchors:

| Documentation need | Prefer |
| --- | --- |
| State fields and meanings | Table of structure fields |
| Function responsibilities | Table of functions and functional roles |
| Enum values and stages | Table of enum members and user-visible meanings |
| Constants and thresholds | Table of constant names, values, units, and behavior |
| Process flow | Mermaid diagram plus explanatory paragraphs |

When source code is relevant, describe why it matters functionally. For example, explain that `write_defconfig()` generates the default build configuration consumed by later `build` and `qemu` commands, not merely that the function exists.

### Diagrams

Use Mermaid for diagrams whenever possible. Use ASCII diagrams only when Mermaid cannot express the idea clearly or when the target renderer cannot support the required diagram.

Choose diagram types by purpose:

| Purpose | Mermaid type |
| --- | --- |
| Process or decision flow | `flowchart` |
| Boot stages or runtime state machines | `stateDiagram-v2` |
| Timing and interaction order | `sequenceDiagram` |
| Data relationships | `erDiagram` or `flowchart` |

Every diagram must be preceded by a paragraph that explains what the reader should learn from it. Every diagram should be followed by a paragraph if it contains important transitions, safety branches, thresholds, or implementation caveats.

### Tables and Lists

Use tables for compact mappings, not as a substitute for explanation. Before a table, explain what the table compares and why the comparison matters. After a table, add a paragraph when there are constraints, defaults, edge cases, or maintenance implications.

Use lists only when order or scannability matters. Ordered lists should represent sequences; unordered lists should represent peer items. Do not place a list immediately after another list, table, diagram, or code block.

## Structure Rules

Apply these rules strictly before finishing any Markdown document. If an existing document violates them, rewrite the affected sections rather than adding small patches that preserve a bad shape.

### Heading Rules

Use exactly one H1 for the document title. Use explicit numeric prefixes in body heading text: H2 headings are `## 1. xxxx`, `## 2. xxxx`; H3 headings are `### 1.1 xxxx`, `### 1.2 xxxx`, `### 2.1 xxxx`, `### 2.2 xxxx`; H4 headings are `#### 1.1.1 xxxx`, `#### 1.1.2 xxxx` when needed. Do not write nested ordered-list style headings like `1. xxxx` followed by an indented `1. xxxx`; the child heading text must include the full dotted number such as `1.1` and `1.2`. Do not use H5 or deeper headings.

Keep heading names short and functional. A heading should usually identify one functional surface or concept, not describe an entire sentence; avoid code symbols, parentheses, and code-like names in headings. Prefer headings such as "启动检查", "平台配置", or "中断处理"; avoid headings such as "`boot_system()` 启动逻辑", "平台配置（含动态探测）", or "构建和测试".

Every parent heading that has child headings must have at least two child headings at that child level. This is a minimum, not a target count. Do not mechanically create exactly two child headings for every parent; organize each level by the real functional shape of the subject. Complex feature areas may need 3 to 4 child headings or more, while simple focused areas can stay at 2. Do not create a single-child hierarchy, and do not leave the body with only H2 sections and no H3 sections. Avoid this shape:

```markdown
## 1. QEMU 运行

### 1.1 配置选择
```

Prefer either adding a second sibling child heading or reorganizing the parent. When the subject naturally contains more distinct behaviors, add all meaningful sibling headings rather than stopping at two:

```markdown
## 1. QEMU 运行

### 1.1 配置选择

### 1.2 系统启动

## 2. 板卡运行

### 2.1 镜像部署

### 2.2 串口连接

### 2.3 启动验证
```

### Block Placement Rules

Never put a diagram, table, list, or code block immediately after a heading. Always insert a paragraph first.

Never put block elements directly next to each other. These patterns are invalid:

````markdown
段落。

| 表格 |
| --- |

```mermaid
flowchart TD
```
````

Insert a paragraph between the table and the diagram that explains the transition from one artifact to the next.

### Paragraph Quality Rules

Write paragraphs that carry real explanatory weight. A good paragraph usually states the functional behavior, the user-visible effect or engineering consequence, and the source-code anchor that controls it.

Avoid weak filler phrases such as "如下所示", "具体见下表", "本节介绍", or "方便阅读". If a phrase does not add domain meaning, remove it or replace it with actual behavior, constraints, or implementation context.

## Final Review Checklist

Before finalizing a Markdown document, check it against this list. Fix violations directly.

### Structure Checks

Confirm that every heading is followed by a paragraph, not by a diagram, table, list, or code block. Confirm that no two block elements are adjacent without an explanatory paragraph between them.

Check that the document has one H1 title and explicit dotted numeric prefixes in body headings, such as `1.`, `1.1`, `1.2`, `2.`, `2.1`, and `2.2`. Confirm the body is not flat with only H2 headings, and confirm every parent heading has at least two child headings when it has any child headings. Also confirm the child count is not mechanically fixed at two across the document; expand complex areas into 3, 4, or more child headings when the functional content warrants it.

Check that heading names are concise, contain no code symbols or parentheses, and usually name one functional surface. Rewrite "X 和 Y" headings into a single broader concept or split them into separate sections when both sides are substantial.

### Technical Grounding Checks

Confirm that functional descriptions include the key code anchors needed for maintenance: important structs, fields, functions, enums, constants, state names, parameter names, or module files. Keep these anchors close to the behavior they explain.

Confirm that diagrams are Mermaid wherever practical, tables have explanatory paragraphs around them, and any code snippets are short and necessary. Remove decorative or navigational sections that do not improve understanding of the system.
