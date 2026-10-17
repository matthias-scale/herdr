const N=c=>`<span class="n">${c}</span>`;
const PIN=N('\u{F0403}'),OCT=N('\u{F0029}'),ZZ=N('\u{F0150}')+'z',CHK=N('\u{F0E1E}'),COG=N('\u{F0493}'),MAG=N('\u{F0349}'),REPO=N('\u{F401}');
const HELP='<span style="color:#8b929e">?</span>';
const ICON={herdr:'<span class="hi" style="color:#EBEDF2">\u{F6036}</span>',scalablev2:'<span class="hi" style="color:#e8eaf0">\u{F6007}</span>',
 'scalable-agent-fleet':'<span class="n" style="color:#79a8f0">\u{F06A9}</span>','agent-harness':'<span class="n" style="color:#e8a54a">\u{F1323}</span>',
 dotfiles:'<span class="n" style="color:#9aa0aa">\u{E615}</span>','obsidian-vault':'<span class="n" style="color:#a98bf0">\u{E6BB}</span>'};
const CHIPC=['k1','k2','k3','k4','k5'];
function badge(name){if(ICON[name])return ICON[name]+' ';const w=name.replace(/[^a-z0-9]/gi,'');return `<span class="k ${CHIPC[w.length%5]}">${(w[0]+w[w.length-1]).toUpperCase()}</span>`}
const L=(s,a='',c='')=>`<span class="l ${c}">${s}${a?`<span class="a">${a}</span>`:''}</span>`;
const DOT={r:'<span class="r">○</span>',g:'<span class="g">◉</span>',b:'<span class="b">●</span>',i:'<span class="m">○</span>',u:HELP};
function R(o,dot,title,tok,age,cls=''){const showTok=o.tok&&tok;const fixed=2+2+(showTok?3:0)+(age?age.length+1:0)+1;let w=o.c-fixed;let t=title;if(t.length>w)t=t.slice(0,Math.max(1,w-1))+'…';
const tt=cls==='f'?`<span class="t">${t}</span>`:`<span class="d">${t}</span>`;
return L(`  ${DOT[dot]} ${tt}${showTok?` <span class="tok">${tok}</span>`:''}`,age,cls)}
function G(o,open,name,count){let n=name;const w=o.c-2-3-1-(count.length+2);if(n.length>w)n=n.slice(0,w-1)+'…';return L(`<span class="m">${open?'⌄':'›'}</span> ${badge(name)} <span class="p">${n}</span> <span class="m">${count}</span>`)}
const W=n=>L(`  <span class="b">●</span> <span class="wk">${n} working ›</span>`,'','half');
function S(open,icon,name,right=''){return L(`<span class="m">${open?'⌄':'›'}</span> <span class="h">${icon} ${name}</span>`,right)}
function RI(o,dot,repo,title,tok,age){const showTok=o.tok&&tok;const fixed=2+2+2+(showTok?3:0)+(age?age.length+1:0)+1;let w=o.c-fixed;let t=title;if(t.length>w)t=t.slice(0,Math.max(1,w-1))+'…';return L(`  ${DOT[dot]} ${badge(repo)}<span class="d">${t}</span>${showTok?` <span class="tok">${tok}</span>`:''}`,age)}
function sub(name){return L(`  ${badge(name)} <span class="m">${name}</span>`)}
function side(o){const r=[];const c=o.c;
r.push(L(`<span class="m">« ${MAG} Search</span>`,'<span class="m">☆ ✎ + …</span>'));
r.push(L(`<span class="m">${o.view} ▾</span> <span class="m">${COG}</span>`,'<span class="m">All projects ▾</span>'));
if(o.menu)r.push(`<span class="pop">${COG} Display<br>☐ provider icon<br>☐ machine name<br>Machines: all hosts ▾</span>`);
r.push(S(1,PIN,'Pinned','1'));
r.push(RI(o,'g','herdr','release checklist','✱1','2h'));
r.push(S(1,OCT,'Blocked','<span class="r">2</span>'));
if(o.view==='Repos'){r.push(RI(o,'r','herdr','approve database change','✱2','8m'));r.push(RI(o,'r','scalable-agent-fleet','worker cleanup approve','⊛1','6m'));}
else{r.push(RI(o,'r','agent-harness','approve database change','✱2','8m'));r.push(RI(o,'r','agent-harness','worker cleanup approve','⊛1','6m'));}
if(o.view==='Repos'){
r.push(G(o,1,'herdr','3 of 7'));
r.push(R(o,'g','fix remote pane restore','✱1','4m','f'));
r.push(R(o,'g','MAT-341 warm channel','✱1','3m'));
r.push(R(o,'u','remote session support','✱1','1m'));
r.push(W(4));
r.push(G(o,1,'scalablev2','4 of 6'));
r.push(R(o,'g','upload dialog QA ready','⊛2','8m'));
r.push(R(o,'u','SCA-4869 photo checklist','✱2','9m'));
r.push(R(o,'i','identify the current model','✱m','1h'));
r.push(R(o,'i','restore quality raw experiments','✱1','2h'));
r.push(W(2));
r.push(G(o,1,'scalable-agent-fleet','2 of 3'));
r.push(R(o,'g','review lane PASS #1702','⊛1','40m'));
r.push(R(o,'u','hago reorg runner','⊛1','22m'));
r.push(W(1));
r.push(G(o,0,'dotfiles','1 of 2'));
r.push(L(`<span class="m">›</span> ${badge('mable')} <span class="p">mable</span> <span class="b">●</span> <span class="wk">3 working</span>`,'','half'));
r.push(L(`<span class="m">⌄</span> <span class="m">${REPO}</span>  <span class="p" style="color:#a2a8b2">No repo yet</span> <span class="m">1 of 1</span>`));
r.push(R(o,'g','flat hunt digest','✱m','1h'));
}else{
r.push(G(o,1,'agent-harness','3 of 6'));
r.push(R(o,'g','fix remote pane restore','✱1','4m','f'));
r.push(R(o,'g','review lane PASS #1702','⊛1','40m'));
r.push(R(o,'u','hago reorg runner','⊛1','22m'));
r.push(W(3));
r.push(G(o,1,'scalable','3 of 5'));
r.push(R(o,'g','upload dialog QA ready','⊛2','8m'));
r.push(R(o,'u','SCA-4869 photo checklist','✱2','9m'));
r.push(R(o,'i','identify the current model','✱m','1h'));
r.push(W(2));
r.push(G(o,0,'personal','1 of 1'));
}
r.push(S(1,ZZ,'Snoozed','1'));
r.push(R(o,'i','design review','✱m','1d'));
r.push(S(0,CHK,'Settled','4'));
return `<div class="term" style="--c:${c}">${r.join('')}</div>`}
const ref='https://ub2.tailc1d69d.ts.net/herdr-sidebar-reference-20261013/';
document.getElementById('cols').innerHTML=
`<div class="col ref"><h2>13 Oct reference</h2><p>T3 screenshot</p><img src="${ref}reference.png" alt="T3 reference screenshot"></div>`+
`<div class="col"><h2>Repos view · 34 cols</h2><p>Provider + machine shown.</p>${side({c:34,view:'Repos',tok:1})}</div>`+
`<div class="col"><h2>Spaces view · 34 cols</h2><p>Same styling for another view.</p>${side({c:34,view:'Spaces',tok:1})}</div>`+
`<div class="col"><h2>Settings open · 30 cols</h2><p>Cog menu: provider and machine off, machine picker.</p>${side({c:30,view:'Repos',tok:0,menu:1})}</div>`;
