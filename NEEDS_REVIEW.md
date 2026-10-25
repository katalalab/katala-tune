# GPU action transport hold

The controller's SSH timeout does not prove that a remote PowerShell action stopped. The stdin receiver rejects malformed or expired requests before evaluating the action body, but no clock-offset bound has been established between controller and target. Do not enable or claim real-host GPU action safety from this fixture-only implementation. Native Windows acceptance must prove bounded receipt, expiry behavior, and post-timeout action state before activation.
