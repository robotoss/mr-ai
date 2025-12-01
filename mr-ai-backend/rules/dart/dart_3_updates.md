LANGUAGE & TECHNOLOGY
---------------------

Primary stack:
- Flutter 3.35 (modern Flutter 3.x)
- Dart 3.8 (modern Dart 3.x with sound null-safety)

Assumptions:
- All Dart code uses sound null-safety.
- Modern Dart 3.x features are available (sealed classes, patterns, records, enhanced enums, extension methods, async/await).
- UI code is written in Flutter widgets (StatelessWidget / StatefulWidget, BuildContext, Material/Cupertino widgets).
- Navigation is typically handled by Flutter routing solutions (e.g., go_router or similar), not by web frameworks or non-Flutter stacks.

When reviewing or proposing changes:
- Treat Dart + Flutter as the default environment for `.dart` files.
- Do NOT switch to Go/Java/TypeScript-style solutions when the file is clearly Dart/Flutter.
- If the file is not Dart (e.g., `.go`, `.ts`, `.kt`), then reason in terms of the actual language in that file.

Dart best practices (3.x):
- Prefer explicit, strongly typed APIs and keep null-safety intact.
- Avoid `dynamic` unless absolutely necessary.
- Use `final` and `const` where possible to express immutability.
- Prefer idiomatic Dart 3.x features (sealed classes, patterns, records) only where they fit the existing project style. Do NOT rewrite the entire design to use new language features if they are not present in the surrounding code.
- Use `async`/`await` for asynchronous work and avoid blocking the main thread.

Flutter best practices:
- Think in terms of Flutter widgets and compositions of small, focused widgets.
- Avoid heavy or long-running work inside `build()`; move such logic to async functions, state management, or initialization hooks.
- Prefer `const` widgets where possible to reduce rebuild cost.
- For `StatefulWidget`:
  - Initialize controllers and animations in `initState`.
  - Dispose of them in `dispose`.
  - Do NOT create controllers or AnimationControllers inside `build()`.

State management:
- Assume the project uses a state management solution common in the Flutter ecosystem (e.g., Provider, Riverpod, Bloc, etc.), but do NOT invent specific providers, notifiers, or blocs that are not visible in the diff or context.
- When suggesting improvements, refer to the existing state-management pattern observed in the code instead of introducing a completely new one.

General rule:
- Prefer concrete, idiomatic Flutter/Dart 3.x suggestions over generic or cross-language advice.
- If you cannot confidently map a suggestion to modern Flutter/Dart practices based on the visible code and context, do NOT speculate.
