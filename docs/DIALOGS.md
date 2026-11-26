# Questions that Panpak asks, with and without `zenity` or `kdialog`

Panpak asks a question in five places. Each one is a window with two buttons.

| Question | When | Buttons |
|---|---|---|
| "{app} doesn't start" | an updated version failed to start twice and an earlier version worked | Go back to {old} / Try again |
| Remove {app} | the menu action "Uninstall" | Remove / Cancel |
| {app} is in protected mode: update | the author reported a serious problem; the update asks for new permissions | Update / Not now |
| Turn the internet back on for {app} | the menu action of protected mode | Turn on / Not now |
| Update {app} | the update asks for new permissions | Update / Not now |

## With `zenity` or `kdialog`

The window of the tool that the desktop uses (`kdialog` on KDE, else `zenity`). Panpak looks for the tool only in `/usr/bin`, `/bin` and `/run/current-system/sw/bin`.

## Without them (Fedora Silverblue, for example)

The question is a notification with the same two buttons: of the notification service of the desktop (`org.freedesktop.Notifications`, which also takes a question that starts from the menu of GNOME) and, if there is none or it shows no buttons, of the notification portal (only for a process that the desktop started as an app; otherwise it would refuse in silence, and nothing is asked). A click on the text of the notification is not an answer and does not start anything: the notification of the service stays. The portal can still refuse in silence from an app scope, when the shell does not know the desktop entry of that app (`InvalidApp` in its journal): nothing is shown, the question stays pending and the next start asks again. If you dismiss the notification, the question stays unanswered at once and the process does not wait any longer.

- A small process waits for a button for **5 minutes** on the notification service of the desktop and **1 minute** on the portal. Then it withdraws the notification, so that no button stays on the screen that would do nothing.
- "Uninstall" moves the process out of the scope of the app before it asks, so that the document portal does not take it for the app. The portal would then refuse the question in silence, so it is not used for "Uninstall": with no notification service nothing is asked, nothing is removed, and the command (`panpak remove <id>`) is told.
- "Uninstall": with no answer nothing is removed, and the question is not kept. Use the menu action again, or run `panpak remove <id>`.
- "{app} doesn't start": the question stays **pending** until a button answers it. At the next start of the app it is asked again, and one line with the command (`panpak rollback <id>`)
  appears on the standard error of the start. The pending question is dropped when it is not the case any more (the version was proven, or you went back by command).
- Protected mode and the update with new permissions: nothing changes without an answer; the next notice asks again.
- With no tool, no notification service, no portal and no session bus: nothing is asked. The commands in the text of the messages (`panpak rollback`, `panpak update`, `panpak allow-internet`,
  `panpak remove`) do the same in a terminal.

Nothing about a question leaves the computer.
