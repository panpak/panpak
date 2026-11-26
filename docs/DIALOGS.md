# Questions that Panpak asks, with and without `zenity` or `kdialog`

Panpak asks a question in five places. Each one is a window with two buttons. A sixth message is not a question: the notification that an app did not start (below).

| Question | When | Buttons |
|---|---|---|
| "{app} doesn't start" | an updated version failed to start twice and an earlier version worked | Go back to {old} / Try again |
| Remove {app} | the menu action "Uninstall" | Remove / Cancel |
| {app} is in protected mode: update | the author reported a serious problem; the update asks for new permissions | Update / Not now |
| Turn the internet back on for {app} | the menu action of protected mode | Turn on / Not now |
| Update {app} | the update asks for new permissions | Update / Not now |

## With `zenity` or `kdialog`

The window of the tool that the desktop uses (`kdialog` on KDE, else `zenity`). Panpak looks for the tool only in `/usr/bin`, `/bin` and `/run/current-system/sw/bin`.

## An app that did not start (a notification, with or without those tools)

Every time an app fails to start (it closes at once with an error code, it crashes, or the system stops it, within 20 seconds), a notification says so: "{app} didn't start", and the reason in one sentence: "The app closed right after starting (error code N).", "The app crashed while starting." or "The app was stopped by the system." The reason is Panpak's own words; it is never taken from what the app printed. The notification is shown as a banner and does not stay in the list of notifications (a `transient` notification), so that when the second failure in a row brings the question "{app} doesn't start" there are not two. Every failed start has its own notification: Panpak does not try to replace one with another (GNOME Shell 51 does not allow that between two processes). The question has the same reason in front of its sentence.

The last part of what the app wrote to its standard error is kept in the file `last-start.log` of the app (in the store, readable by you only, 16 KiB at most, emptied at each start, removed with the app), and the notification has the button **Show details** that opens it (when the app wrote something: with nothing in the log there is no button). The log begins with a line of Panpak's, then the text of the app. The log can hold personal data: do not send it anywhere without reading it. When you start the app from a terminal nothing is captured and nothing is shown as a notification: the reason is written in the terminal. With no notification service nothing is shown, and the log stays.

## After "Go back" (with or without those tools)

After **Go back** a notification, titled with the name of the app, says that the app is back to the version ("{app} is back to version {old}. Your documents stay as they are.") or that it could not go back and why ("{app} could not go back to version {old}: {reason}"). After **Try again** nothing is said: the app starts again, and that is the answer.

## Without them (Fedora Silverblue, for example)

The question is a notification with the same two buttons: of the notification service of the desktop (`org.freedesktop.Notifications`, which also takes a question that starts from the menu of GNOME) and, if there is none or it shows no buttons, of the notification portal (only for a process that the desktop started as an app; otherwise it would refuse in silence, and nothing is asked). A click on the text of the notification is not an answer and does not start anything: the notification of the service stays. The portal can still refuse in silence from an app scope, when the shell does not know the desktop entry of that app (`InvalidApp` in its journal): nothing is shown, the question stays pending and the next start asks again. If you dismiss the notification, the question stays unanswered at once and the process does not wait any longer.

- A small process waits for a button for **5 minutes** on the notification service of the desktop and **1 minute** on the portal. Then it withdraws the notification, so that no button stays on the screen that would do nothing.
- "Uninstall" moves the process out of the scope of the app before it asks, so that the document portal does not take it for the app. The portal would then refuse the question in silence, so it is not used for "Uninstall": with no notification service nothing is asked, nothing is removed, and the command (`panpak remove <id>`) is told.
- "Uninstall": with no answer nothing is removed, and the question is not kept. Use the menu action again, or run `panpak remove <id>`.
- "{app} doesn't start": the question stays **pending** until a button answers it. At the next start of the app it is asked again, and one line with the command (`panpak rollback <id>`)
  appears on the standard error of the start. The pending question is dropped when it is not the case any more (the version was proven, or you went back by command).
- Protected mode and the update with new permissions: nothing changes without an answer; the next notice asks again. The button "Update now" answers only when the press comes from the notification service of the desktop: a signal that another program of the session sends is ignored.
- Known limit (postponed until after the beta): if a question is not answered, its notification stays in the list of notifications of the desktop, and the question comes back at the next start of the app. Another start while the question still waits does not show the notification again: the desktop (GNOME Shell 51, measured on 2026-11-26) accepts a new `replaces_id` only from the process that made the notification, as long as that process is alive. To find a question that you did not see, open the list of notifications, or start the app again after the wait (5 minutes on the service, 1 minute on the portal).
- With no tool, no notification service, no portal and no session bus: nothing is asked. The commands in the text of the messages (`panpak rollback`, `panpak update`, `panpak allow-internet`,
  `panpak remove`) do the same in a terminal.

Nothing about a question leaves the computer.
