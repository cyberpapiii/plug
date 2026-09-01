# Plug 0.7.3

Plug.app no longer mistakes unrelated background jobs with `plug` in their
name for Plug's daemon. This fixes first-run adoption on machines where another
application uses a label such as `local.claude-rc.plug`.

Unknown jobs remain untouched. The app only replaces a service after its
executable or ServiceManagement metadata proves that Plug owns it.
