// Ferry GNOME Shell extension (GNOME 45+).
// - panel menu: send clipboard / files to my devices or nearby devices, pair,
//   Incoming (transfers from unpaired devices) with Accept / Decline / Block
// - clipboard bridge: GNOME Wayland does not let background apps read or write
//   the clipboard, so the shell (which can) forwards changes to the daemon.
//   Event driven (Meta.Selection 'owner-changed'), no polling.

import GLib from 'gi://GLib';
import Gio from 'gi://Gio';
import GObject from 'gi://GObject';
import St from 'gi://St';
import Clutter from 'gi://Clutter';
import Meta from 'gi://Meta';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';
import * as ModalDialog from 'resource:///org/gnome/shell/ui/modalDialog.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const SOCKET = GLib.build_filenamev([GLib.get_user_runtime_dir(), 'ferry.sock']);
const PASSWORD_HINT = 'x-kde-passwordManagerHint';

function esc(s) {
    return String(s).replace(/\\/g, '\\\\').replace(/\t/g, '\\t').replace(/\n/g, '\\n').replace(/\r/g, '\\r');
}

function unesc(s) {
    return s.replace(/\\(.)/g, (_, c) => ({t: '\t', n: '\n', r: '\r'})[c] ?? c);
}

function vbox(style) {
    const b = new St.BoxLayout({style});
    if ('orientation' in b)
        b.orientation = Clutter.Orientation.VERTICAL;
    else
        b.vertical = true;
    return b;
}

/** Line-based connection to the ferry daemon with automatic reconnect. */
class Link {
    constructor(onLine, onState) {
        this._onLine = onLine;
        this._onState = onState;
        this._retry = 2;
        this._timer = 0;
        this._conn = null;
        this._out = null;
        this._cancel = null;
        this._dead = false;
    }

    start() {
        if (this._dead)
            return;
        const client = new Gio.SocketClient();
        const cancel = new Gio.Cancellable();
        this._cancel = cancel;
        client.connect_async(Gio.UnixSocketAddress.new(SOCKET), cancel, (c, res) => {
            try {
                this._conn = c.connect_finish(res);
            } catch (e) {
                this._scheduleRetry();
                return;
            }
            this._retry = 2;
            this._out = this._conn.get_output_stream();
            this._in = new Gio.DataInputStream({base_stream: this._conn.get_input_stream()});
            this.send('subscribe', 'clipboard');
            this.send('status');
            this._onState(true);
            this._read();
        });
    }

    _read() {
        const cancel = this._cancel;
        this._in.read_line_async(GLib.PRIORITY_DEFAULT, cancel, (s, res) => {
            let line = null;
            try {
                [line] = s.read_line_finish_utf8(res);
            } catch (e) {
                line = null;
            }
            if (line === null) {
                this._closed();
                return;
            }
            try {
                this._onLine(line.split('\t').map(unesc));
            } catch (e) {
                console.error(`ferry: ${e}`);
            }
            this._read();
        });
    }

    send(...fields) {
        if (!this._out)
            return false;
        try {
            const data = new TextEncoder().encode(`${fields.map(esc).join('\t')}\n`);
            this._out.write_all(data, null);
            return true;
        } catch (e) {
            this._closed();
            return false;
        }
    }

    _closed() {
        if (this._conn) {
            try {
                this._conn.close(null);
            } catch (e) {}
        }
        this._conn = null;
        this._out = null;
        this._onState(false);
        this._scheduleRetry();
    }

    _scheduleRetry() {
        if (this._dead || this._timer)
            return;
        this._timer = GLib.timeout_add_seconds(GLib.PRIORITY_LOW, this._retry, () => {
            this._timer = 0;
            this.start();
            return GLib.SOURCE_REMOVE;
        });
        this._retry = Math.min(this._retry * 2, 60);
    }

    stop() {
        this._dead = true;
        if (this._timer)
            GLib.source_remove(this._timer);
        this._timer = 0;
        this._cancel?.cancel();
        if (this._conn) {
            try {
                this._conn.close(null);
            } catch (e) {}
        }
        this._conn = null;
        this._out = null;
    }
}

function header(text) {
    const h = new PopupMenu.PopupMenuItem(text, {reactive: false});
    h.label.style = 'font-size: 0.85em; font-weight: bold; opacity: 0.7;';
    return h;
}

const FerryIndicator = GObject.registerClass(
class FerryIndicator extends PanelMenu.Button {
    _init(ext) {
        super._init(0.0, 'Ferry');
        this._ext = ext;
        this._icon = new St.Icon({icon_name: 'phone-symbolic', style_class: 'system-status-icon'});
        this.add_child(this._icon);

        this._status = new PopupMenu.PopupMenuItem('Ferry is not running', {reactive: false});
        this.menu.addMenuItem(this._status);

        // Incoming: transfers from unpaired devices waiting for a decision (hidden when empty)
        this._incomingSep = new PopupMenu.PopupSeparatorMenuItem();
        this.menu.addMenuItem(this._incomingSep);
        this._incoming = new PopupMenu.PopupMenuSection();
        this.menu.addMenuItem(this._incoming);
        this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());

        this._clipItem = new PopupMenu.PopupMenuItem('Send clipboard to my devices');
        this._clipItem.connect('activate', () => ext.sendClipboard());
        this.menu.addMenuItem(this._clipItem);

        this._sendMenu = new PopupMenu.PopupSubMenuMenuItem('Send files to');
        this.menu.addMenuItem(this._sendMenu);
        this._setDevices(null);

        this._pairItem = new PopupMenu.PopupMenuItem('Pair a new device…');
        this._pairItem.connect('activate', () => ext.link.send('pairshow'));
        this.menu.addMenuItem(this._pairItem);

        this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());

        this._auto = new PopupMenu.PopupSwitchMenuItem('Sync clipboard with my devices', true);
        this._auto.connect('toggled', (_i, state) =>
            ext.link.send('set', 'auto_clipboard', state ? 'on' : 'off'));
        this.menu.addMenuItem(this._auto);

        this._visible = new PopupMenu.PopupSwitchMenuItem('Visible to nearby devices', true);
        this._visible.connect('toggled', (_i, state) =>
            ext.link.send('set', 'visible', state ? 'on' : 'off'));
        this.menu.addMenuItem(this._visible);

        this._openItem = new PopupMenu.PopupMenuItem('Open received files');
        this._openItem.connect('activate', () => {
            if (ext.downloadDir)
                Gio.AppInfo.launch_default_for_uri(GLib.filename_to_uri(ext.downloadDir, null), null);
        });
        this.menu.addMenuItem(this._openItem);

        this.menu.connect('open-state-changed', (_m, open) => {
            if (!open)
                return;
            ext.link.send('status');
            ext.link.send('incoming');
            ext.link.send('devices'); // short network scan, the submenu fills in when answers arrive
        });
        this._setIncoming([]);
        this.setOnline(false);
    }

    setOnline(online) {
        for (const i of [this._clipItem, this._sendMenu, this._pairItem, this._auto, this._visible, this._openItem])
            i.setSensitive(online);
        this._icon.opacity = online ? 255 : 120;
        if (!online)
            this._status.label.text = 'Ferry is not running';
    }

    setStatus(name, peers, auto, visible, nIncoming) {
        const list = peers.length ? peers.join(', ') : 'nothing paired yet';
        this._status.label.text = `${name}  ⇄  ${list}`;
        this._auto.setToggleState(auto);
        this._visible.setToggleState(visible);
        this._icon.icon_name = nIncoming > 0 ? 'mail-unread-symbolic' : 'phone-symbolic';
    }

    setToggle(key, on) {
        if (key === 'auto_clipboard')
            this._auto.setToggleState(on);
        else if (key === 'visible')
            this._visible.setToggleState(on);
    }

    /** devices: null = still scanning, else [{id, name, kind, paired, online}] */
    _setDevices(devices) {
        const m = this._sendMenu.menu;
        m.removeAll();
        if (devices === null) {
            m.addMenuItem(header('Looking for devices…'));
            return;
        }
        const mine = devices.filter(d => d.paired);
        const near = devices.filter(d => !d.paired);
        m.addMenuItem(header('My devices'));
        if (!mine.length)
            m.addMenuItem(header('  none paired yet'));
        mine.forEach((d, i) => {
            const label = `${i === 0 ? '★ ' : ''}${d.name}${d.online ? '' : '  (offline - will be queued)'}`;
            const it = new PopupMenu.PopupMenuItem(label);
            it.connect('activate', () => this._ext.pickAndSend(d.id));
            m.addMenuItem(it);
        });
        m.addMenuItem(header('Nearby - they have to accept'));
        if (!near.length)
            m.addMenuItem(header('  none found'));
        for (const d of near) {
            const it = new PopupMenu.PopupMenuItem(`${d.name}  (${d.kind})`);
            it.connect('activate', () => this._ext.pickAndSend(d.id));
            m.addMenuItem(it);
        }
    }

    setDevices(devices) {
        this._setDevices(devices);
    }

    /** items: [{id, from, summary}] */
    _setIncoming(items) {
        this._incoming.removeAll();
        const any = items.length > 0;
        this._incomingSep.visible = any;
        this._incoming.actor.visible = any;
        if (!any)
            return;
        this._incoming.addMenuItem(header(`Incoming from devices that are not paired (${items.length})`));
        for (const it of items) {
            const sub = new PopupMenu.PopupSubMenuMenuItem(`${it.from}: ${it.summary}`);
            const add = (label, cmd) => {
                const x = new PopupMenu.PopupMenuItem(label);
                x.connect('activate', () => this._ext.link.send(cmd, it.id));
                sub.menu.addMenuItem(x);
            };
            add('Accept', 'accept');
            add('Decline', 'decline');
            add(`Decline and block ${it.from}`, 'block');
            this._incoming.addMenuItem(sub);
        }
    }

    setIncoming(items) {
        this._setIncoming(items);
    }
});

export default class FerryExtension extends Extension {
    enable() {
        this.downloadDir = null;
        this._lastSet = null;
        this._pairDialog = null;
        this._st = {name: '', peers: [], auto: true, visible: true, nIncoming: 0};
        this._devs = [];
        this._inc = [];
        this._indicator = new FerryIndicator(this);
        Main.panel.addToStatusArea(this.uuid, this._indicator);

        this.link = new Link(f => this._onLine(f), online => {
            this._indicator?.setOnline(online);
            if (online)
                this.link.send('incoming');
        });
        this.link.start();

        this._selection = global.display.get_selection();
        this._selId = this._selection.connect('owner-changed', (_sel, type, source) => {
            if (type !== Meta.SelectionType.SELECTION_CLIPBOARD || !source)
                return;
            try {
                if (source.get_mimetypes().includes(PASSWORD_HINT))
                    return; // never sync passwords from password managers
            } catch (e) {}
            St.Clipboard.get_default().get_text(St.ClipboardType.CLIPBOARD, (_c, text) => {
                if (text && text !== this._lastSet)
                    this.link.send('clipchanged', text);
            });
        });
    }

    disable() {
        if (this._selId)
            this._selection.disconnect(this._selId);
        this._selId = 0;
        this._selection = null;
        this._closePair();
        this.link?.stop();
        this.link = null;
        this._indicator?.destroy();
        this._indicator = null;
    }

    _updateStatus() {
        const s = this._st;
        this._indicator?.setStatus(s.name, s.peers, s.auto, s.visible, s.nIncoming);
    }

    _onLine(f) {
        switch (f[0]) {
        case 'setclip':
            this._lastSet = f[1];
            St.Clipboard.get_default().set_text(St.ClipboardType.CLIPBOARD, f[1]);
            break;
        case 'status':
            this._st.name = f[1];
            this.downloadDir = f[3];
            this._st.auto = f[4] === '1';
            this._st.visible = f[7] !== '0';
            this._st.nIncoming = parseInt(f[8] || '0');
            this._st.peers = [];
            break;
        case 'peer':
            this._st.peers.push(f[2]);
            break;
        case 'end':
            this._updateStatus();
            break;
        case 'dev':
            this._devs.push({id: f[1], name: f[2], kind: f[3], paired: f[4] === 'paired', online: f[5] === '1'});
            break;
        case 'devend':
            this._indicator?.setDevices(this._devs);
            this._devs = [];
            break;
        case 'in':
            this._inc.push({id: f[1], from: f[2], summary: f[3]});
            break;
        case 'inend':
            this._indicator?.setIncoming(this._inc);
            this._st.nIncoming = this._inc.length;
            this._updateStatus();
            this._inc = [];
            break;
        case 'incoming':
            this.link.send('incoming');
            break;
        case 'devices':
        case 'paired':
            this._closePair();
            this.link.send('status');
            break;
        case 'config':
            this._indicator?.setToggle(f[1], f[2] === '1');
            break;
        case 'pair':
            this._showPair(f[1], f[3]);
            break;
        case 'pairfailed':
            this._closePair();
            Main.notify('Ferry', f[1]);
            break;
        case 'ok':
            if (f[1]?.startsWith('clipboard sent'))
                Main.notify('Ferry', f[1].charAt(0).toUpperCase() + f[1].slice(1));
            break;
        }
    }

    sendClipboard() {
        St.Clipboard.get_default().get_text(St.ClipboardType.CLIPBOARD, (_c, text) => {
            if (text)
                this.link.send('sendclip', text);
            else
                Main.notify('Ferry', 'The clipboard does not contain text.');
        });
    }

    pickAndSend(target) {
        let proc;
        try {
            proc = Gio.Subprocess.new(
                ['zenity', '--file-selection', '--multiple', '--separator=\n', '--title=Send with Ferry'],
                Gio.SubprocessFlags.STDOUT_PIPE | Gio.SubprocessFlags.STDERR_SILENCE);
        } catch (e) {
            Main.notify('Ferry', 'Install "zenity" to pick files, or right-click files in Files → Scripts → Send to phone.');
            return;
        }
        proc.communicate_utf8_async(null, null, (p, res) => {
            try {
                const [, out] = p.communicate_utf8_finish(res);
                if (!p.get_successful() || !out)
                    return;
                const files = out.split('\n').filter(s => s.length);
                if (files.length)
                    this.link.send('sendfiles', target ?? '', ...files);
            } catch (e) {
                console.error(`ferry: ${e}`);
            }
        });
    }

    _showPair(code, addrs) {
        this._closePair();
        const d = new ModalDialog.ModalDialog({destroyOnClose: true});
        const box = vbox('spacing: 14px; padding: 8px 16px; min-width: 360px;');
        box.add_child(new St.Label({text: 'Pair a device', style: 'font-weight: bold; font-size: 15pt;'}));
        box.add_child(new St.Label({text: 'On the phone (or the other PC) choose “Pair with computer” and enter:'}));
        const shown = addrs ? addrs.split(',').join('   or   ') : '(no network address found)';
        box.add_child(new St.Label({text: `Address:  ${shown}`, style: 'font-size: 13pt;'}));
        box.add_child(new St.Label({
            text: code,
            style: 'font-family: monospace; font-size: 30pt; font-weight: bold; padding: 6px 0;',
            x_align: Clutter.ActorAlign.CENTER,
        }));
        box.add_child(new St.Label({
            text: 'Paired devices share the clipboard and send files without asking.\nThe code is valid for 5 minutes.',
            style: 'color: #999;',
        }));
        d.contentLayout.add_child(box);
        d.setButtons([{
            label: 'Cancel',
            key: Clutter.KEY_Escape,
            action: () => {
                this.link?.send('pairstop');
                this._closePair();
            },
        }]);
        d.connect('closed', () => {
            if (this._pairDialog === d)
                this._pairDialog = null;
        });
        this._pairDialog = d;
        d.open();
    }

    _closePair() {
        const d = this._pairDialog;
        this._pairDialog = null;
        if (d)
            d.close();
    }
}
