---
description: Create or refine a reusable Agent skill when the user wants a repeatable workflow captured in the workspace.
---

# Create a Skill

1. Understand the reusable task, its expected result, and a few representative requests that should activate the skill. Ask only for information that materially changes the workflow.
2. Choose a short, action-oriented ID using lowercase letters, digits, and hyphens. The ID must be no longer than 64 characters.
3. Write a concise description that states both what the skill does and when it should be used.
4. Write the workflow as clear Markdown instructions. Preserve any requirements or tool references the user explicitly requests. Otherwise, work with tools that are currently available and never invent unavailable tools.
5. Save the skill as `skills/{id}.md` with YAML front matter containing `description`, followed by the Markdown instructions.
6. For an existing skill, inspect its current contents first and preserve anything the user did not ask to change. Never replace an existing skill while handling an unrelated creation request.
