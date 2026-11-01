// Prueba de humo del panel en un navegador real (Playwright).
// Entra con el token, recorre todas las vistas del menú y falla si hay excepciones de JavaScript
// o errores de consola ajenos a la red (las fuentes de Google no siempre cargan dentro de Docker).
// Uso: node scripts/ui-smoke.js http://127.0.0.1:7681 TOKEN
const { chromium } = require('playwright');

const [, , base, token] = process.argv;
if (!base || !token) {
    console.error('uso: node ui-smoke.js URL TOKEN');
    process.exit(2);
}

const ignorable = [/Failed to load resource/, /fonts\.googleapis|fonts\.gstatic|cdn\.jsdelivr/, /net::ERR_/];

(async () => {
    const browser = await chromium.launch({ args: ['--no-sandbox'] });
    const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
    const problems = [];
    let view = 'inicio';

    page.on('pageerror', (e) => problems.push(`[${view}] excepción: ${e.message}`));
    page.on('console', (m) => {
        if (m.type() === 'error' && !ignorable.some((re) => re.test(m.text()))) {
            problems.push(`[${view}] consola: ${m.text()}`);
        }
    });

    // El token va en el fragmento: la página lo guarda y recarga sola.
    await page.goto(`${base}/#t=${token}`);
    await page.waitForSelector('#nav a', { timeout: 20000 });

    const views = await page.$$eval('#nav a', (as) => as.map((a) => ({ href: a.getAttribute('href'), label: a.textContent.trim() })));
    if (views.length < 5) problems.push(`el menú trae ${views.length} vistas; se esperaban al menos 5`);

    for (const v of views) {
        view = v.label;
        await page.click(`#nav a[href="${v.href}"]`);
        await page.waitForTimeout(800);
        const activo = await page.$(`#nav a.on[href="${v.href}"]`);
        if (!activo) problems.push(`[${view}] la vista no quedó activa`);
        console.log(`ok    ${view}`);
    }

    // sin sesión, la página no muestra datos: vuelve a entrar sin token y no debe romperse
    await page.evaluate(() => localStorage.clear());
    view = 'sin token';
    await page.goto(base + '/');
    await page.waitForTimeout(800);

    await browser.close();
    if (problems.length) {
        console.log('FALLA:\n  ' + problems.join('\n  '));
        process.exit(1);
    }
    console.log(`todas las vistas cargan sin errores (${views.length})`);
})().catch((e) => {
    console.error('FALLA:', e.message);
    process.exit(1);
});
