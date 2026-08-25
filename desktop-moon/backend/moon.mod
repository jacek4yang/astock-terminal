name = "astock/desktop_backend"

version = "6.0.0"

import {
  "astock/desktop_shared@6.0.0",
  "moonbitlang/async@0.21.0",
  "moonbit-community/proton@0.2.1",
  "moonbit-community/proton_contract@0.2.1",
}

rule(
  name: "proton_codegen",
  command: "moonx moonbit-community/proton_codegen@0.2.1 -C \"$mod_dir\" \"$input\" -o \"$output\"",
)

options(
  warn_list: "",
  preferred_target: "native",
  supported_targets: "+native",
)
