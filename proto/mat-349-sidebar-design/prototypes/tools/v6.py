s=open('/home/ubuntu/shots349/v5/index.html').read()
s=s.replace('<style>','<style>\n@font-face{font-family:"NerdSym";src:url(nerd.woff2) format("woff2")}\n@font-face{font-family:"HerdrIcons";src:url(herdr-icons.otf) format("opentype")}\n.n,.hi{display:inline-block;width:1ch;margin-right:.45ch;text-align:center;overflow:visible;font-weight:400}.n{font-family:"NerdSym"}.hi{font-family:"HerdrIcons"}',1)
s=s.replace('font:13px/1.45 "JetBrains Mono"','font:13px/1.45 "JetBrains Mono","SFMono-Regular",Menlo')
a=s.index('<script>');b=s.index('</script>')
js=open('/home/ubuntu/shots349/tools/v6.js').read()
s=s[:a]+'<script>\n'+js+s[b:]
s=s.replace('.tok{color:#c98a5a}','.tok{color:#c98a5a}.wk{color:#8fb4ec}.half{opacity:.5}.k{border-radius:3px;font-size:11px;line-height:1.3}')
s=s.replace('round 5</h1>','round 7 (approved)</h1>').replace('sidebar restyle round 5','sidebar restyle round 7')
n0=s.index('<ul class="notes">');n1=s.index('</ul>',n0)+5
s=s[:n0]+open('/home/ubuntu/shots349/tools/v6-notes.html').read()+s[n1:]
open('/home/ubuntu/shots349/v6/index.html','w').write(s)
