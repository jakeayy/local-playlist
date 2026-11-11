# Adding cookies manually

Local Playlist can log in to YouTube and SoundCloud through their normal sign-in page
(**Settings → Accounts → Log in**). If that doesn't work for you, for example because
Google says *"This browser or app may not be secure"*, or if you'd rather not sign in
inside the app, you can paste cookies from a browser where you're already logged in.

> [!WARNING]
> Session cookies give **full access to your account**. Only paste them into Local
> Playlist on your own computer, and never share them with anyone or with any website
> or server. Local Playlist stores them encrypted on your device (the key is kept in your
> system keyring) and sends them only to the platform they belong to.

## What the app needs

| Platform   | Required cookie                     | Accepted formats                                 |
| ---------- | ----------------------------------- | ------------------------------------------------ |
| YouTube    | `SAPISID` or `__Secure-3PAPISID`    | `cookies.txt` export, or a full `Cookie:` header |
| SoundCloud | `oauth_token`                       | `cookies.txt` export, a `Cookie:` header, or just `oauth_token=<value>` |

For YouTube, paste **all** `youtube.com` cookies (an export or the full header), not
only `SAPISID`. YouTube checks several cookies together.

## Option 1: export with a browser extension (easiest)

1. Install an extension that exports cookies in the Netscape `cookies.txt` format and
   runs entirely locally, for example:
   - Chrome / Edge / Brave: **Get cookies.txt LOCALLY**
   - Firefox: **cookies.txt**
2. **YouTube tip:** open a **private / incognito window**, sign in to
   <https://www.youtube.com> there, and export from that window. Then **close the
   window without signing out**. Signing out invalidates the cookies, and a normal tab
   keeps rotating them, which can expire the copy you exported.
3. On the site (`youtube.com` or `soundcloud.com`), open the extension and export
   cookies **for the current site**.
4. Open the exported file in a text editor and copy its contents.
5. In Local Playlist: **Settings → Accounts → Paste cookies…**, paste, and click
   **Save cookies**.

## Option 2: copy from the browser's developer tools

### YouTube

1. Sign in at <https://www.youtube.com> (ideally in a private window, see the tip above).
2. Press <kbd>F12</kbd> to open developer tools and go to the **Network** tab.
3. Reload the page and click any request to `www.youtube.com` (for example the first
   document request).
4. Under **Request Headers**, find `Cookie:` and copy its whole value.
5. Paste it into **Paste cookies…** (with or without the `Cookie:` prefix).

### SoundCloud

1. Sign in at <https://soundcloud.com>.
2. Press <kbd>F12</kbd> and open:
   - **Application → Cookies → https://soundcloud.com** in Chrome / Edge, or
   - **Storage → Cookies → https://soundcloud.com** in Firefox.
3. Find the `oauth_token` cookie and copy its value.
4. Paste `oauth_token=<value>` into **Paste cookies…**.

## Troubleshooting

- **"No … session cookie found"**: the pasted text doesn't contain the required cookie.
  Make sure you were logged in and exported cookies for the right site.
- **"YouTube didn't accept the saved login"**: the cookies expired or were rotated.
  Export them again (the private-window tip above helps them last).
- **A playlist on another channel isn't found**: if your Google account has several
  channels (Brand Accounts), pick the right one under **Settings → Accounts → Channel**.
  Imports also try your other channels automatically.

## Removing them

**Settings → Accounts → Log out** deletes the stored cookies from your device. To also
end the session on the platform's side, sign out of that session on the website.
