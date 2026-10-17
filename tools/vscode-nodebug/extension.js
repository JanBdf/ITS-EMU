// The CDT GDB adapter ignores `noDebug`, so Run Without Debugging attaches
// like a debug session and stops at the `tbreak` of the launch configuration.
// This drops those commands when the session is started without debugging.
const vscode = require('vscode');

const TBREAK = /^\s*tbreak\b/;

function withoutBreaks(config) {
    if (!config.noDebug) return config;
    for (const key of ['initCommands', 'customResetCommands']) {
        if (Array.isArray(config[key])) config[key] = config[key].filter((cmd) => !TBREAK.test(cmd));
    }
    return config;
}

function activate(context) {
    context.subscriptions.push(
        vscode.debug.registerDebugConfigurationProvider('gdbtarget', {
            resolveDebugConfigurationWithSubstitutedVariables: (_folder, config) => withoutBreaks(config),
        }),
    );
}

module.exports = { activate, withoutBreaks };
