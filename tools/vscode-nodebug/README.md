Makes **Run Without Debugging** (Ctrl+F5) of `gdbtarget` launch
configurations start the program without stopping at `main`: when VS Code
starts a session without debugging, the extension removes the `tbreak`
commands from `initCommands` and `customResetCommands`. F5 is unchanged.
Breakpoints set in the editor still stop the program.

```sh
tools/vscode-nodebug/package.sh          # builds gdbtarget-nodebug.vsix
code --install-extension tools/vscode-nodebug/gdbtarget-nodebug.vsix
code --uninstall-extension its-emu.gdbtarget-nodebug
```
