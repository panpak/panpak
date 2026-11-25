# 0009. Questions without a window tool: a notification of the portal

- Status: accepted
- Date: 2026-11-24
- Pull requests: #108

## Context
Fedora Silverblue has neither `zenity` nor `kdialog`. The window of kept versions, "Uninstall", protected mode and the question of new permissions did not appear, and the question was lost.

## Decision
When neither tool is installed, the question goes as a notification of the desktop portal (`org.freedesktop.portal.Notification`) with two buttons and no default action. A small process waits for the click for up to 5 minutes, then withdraws the notification, so that no dead buttons remain. The question of kept versions stays pending (`apps/<id>/pending-question`) and is asked again at the next start. "Uninstall" does not stay pending: with no answer nothing is done.

## Alternatives that were not chosen
A window drawn by Panpak (the Panpak app is step 10b); a question in the terminal only.

## Consequences
The click on a real notification was not tried on a real desktop when this was written (planned on the VM). Detached processes that wait for a click hold one connection to the bus for up to 5 minutes.

## Sources
docs/DIALOGS.md; PANPAK-FORMAT.md §6.10; pull request #108.
