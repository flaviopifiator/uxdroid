#!/bin/bash
# Proves packaged ART runs a DEX main() as compiled code in a clean system.
set -u
source /work/runtime/art/run.sh
ART=("$RUNTIME/bin/dalvikvm64" -Xbootclasspath:"$BOOT" -Xbootclasspath-locations:"$BOOT" -Ximage:"$RUNTIME/framework/boot.art" -Xusejit:false)
clock() { local start end; start=$(date +%s%N); "$@" >/tmp/out 2>/tmp/err; local rc=$?; end=$(date +%s%N); echo "$(( (end-start)/1000000 )) ms rc=$rc: $(tail -1 /tmp/out | cut -c1-70)"; }
echo "aosp tree present: $([ -d /aosp ] && echo yes || echo no)"
"${ART[@]}" -cp "$RUNTIME/samples/Hello.jar" Hello 2>&1 | grep -vE "heap.cc|parsed_options|ICU file|timezone" | tail -5 | cut -c1-220
for run in 1 2 3; do echo "Hello compiled:      $(clock "${ART[@]}" -cp "$RUNTIME/samples/Hello.jar" Hello)"; done
for run in 1 2 3; do echo "Compute compiled:    $(clock "${ART[@]}" -cp "$RUNTIME/samples/Compute.jar" Compute)"; done
echo "Compute interpreted: $(clock "${ART[@]}" -Xint -cp "$RUNTIME/samples/Compute.jar" Compute)"
if [ -x "$RUNTIME/bin/oatdump64" ]; then
    "$RUNTIME/bin/oatdump64" --oat-file="$RUNTIME/samples/oat/x86_64/Compute.odex" --no-disassemble 2>/dev/null | grep -E "OatMethodOffsets|CODE: \(code_offset=0x[1-9a-f]|Compute.main|compiler-filter|INSTRUCTION SET" -m8 | cut -c1-120
fi
du -sh "$RUNTIME" "$RUNTIME/lib64" "$RUNTIME/framework"
ls "$RUNTIME/bin"
