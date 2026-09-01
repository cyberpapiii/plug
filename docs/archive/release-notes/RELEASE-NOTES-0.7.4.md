# Plug 0.7.4

Plug.app now tolerates normal macOS launchd churn during first-run setup.
Short-lived unrelated system jobs can disappear between discovery and
inspection; they no longer block Plug's daemon adoption.

Plug also distinguishes its normal foreground application process from its
background daemon, so simply having Plug.app open no longer creates an
ambiguous ownership warning.

The app now also carries forward proof of the old command-line launchd path
after repairing its shell link, so existing CLI-managed installations can be
adopted instead of being reported as unknown.

The exact `com.plug.daemon` service remains fail-closed and is never replaced
without proven Plug ownership.
