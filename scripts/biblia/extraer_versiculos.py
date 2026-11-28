#!/usr/bin/env python3
"""Genera los versículos del día que la barra superior lleva incrustados.

Se corre una sola vez, fuera de la app. La app no lee ninguna base de datos:
solo el archivo de texto que produce este script (unos 50 KB), compilado
dentro del ejecutable.

Fuente: https://github.com/develop4God/bible_versions, carpeta es/
(formato MyBible: tablas books y verses, con marcas como <t>, <J>, <f>).

Uso:
    git clone --depth 1 https://github.com/develop4God/bible_versions
    python3 scripts/biblia/extraer_versiculos.py bible_versions/es/NVI_es.SQLite3.gz

NVI: © 1986, 1999, 2015 Biblica, Inc. Biblica permite citar hasta 500
versículos sin permiso escrito, con la nota de copyright; por eso el
resultado se limita a MAX_VERSICULOS.
"""

from __future__ import annotations

import argparse
import gzip
import re
import shutil
import sqlite3
import sys
import tempfile
import unicodedata
from pathlib import Path

RAIZ = Path(__file__).resolve().parents[2]
REFERENCIAS = Path(__file__).with_name("referencias.txt")
SALIDA = RAIZ / "crates" / "muelle-core" / "src" / "versiculos_nvi.tsv"

# Uno por día del año; muy por debajo de los 500 que permite Biblica.
MAX_VERSICULOS = 366
# Más largo no se lee bien pasando por la barra, ni en cinco frases.
MAX_CARACTERES = 240

# Nombres que se escriben distinto en la lista y en la base.
ALIAS = {
    "cantares": "cantar de los cantares",
    "nahum": "nahum",
    "hechos": "hechos",
    "apocalipsis": "apocalipsis",
}

CITA = re.compile(r"^(?P<libro>.+?)\s+(?P<cap>\d+):(?P<desde>\d+)(?:-(?P<hasta>\d+))?$")


def sin_tildes(texto: str) -> str:
    normal = unicodedata.normalize("NFD", texto.casefold())
    return "".join(c for c in normal if unicodedata.category(c) != "Mn").strip()


def limpiar(texto: str) -> str:
    """Quita notas, títulos y marcas de formato MyBible."""
    # Notas al pie, números de nota y encabezados: fuera con su contenido.
    texto = re.sub(r"<f>.*?</f>", "", texto, flags=re.S)
    texto = re.sub(r"<n>.*?</n>", "", texto, flags=re.S)
    texto = re.sub(r"<e>.*?</e>", "", texto, flags=re.S)
    texto = re.sub(r"<S>.*?</S>", "", texto, flags=re.S)
    # Resto de marcas (<t>, <J>, <i>, <pb/>): se queda el texto.
    texto = re.sub(r"<[^>]+>", "", texto)
    texto = re.sub(r"\s+", " ", texto).strip()
    texto = re.sub(r"\s+([,;:.!?»])", r"\1", texto)
    return texto


def comillas(texto: str) -> str:
    """En la Biblia las comillas de diálogo se abren en un versículo y se
    cierran en otro; sueltas en la barra se ven como un error."""
    texto = texto.lstrip("»«“”\"' ").rstrip("«“ ")
    if texto.count("«") != texto.count("»"):
        texto = texto.replace("«", "").replace("»", "")
    if texto.count("“") != texto.count("”"):
        texto = texto.replace("“", "").replace("”", "")
    return texto.strip()


def como_frase(texto: str) -> str:
    """Un versículo suelto se lee como frase completa: sin puntuación al
    principio, con mayúscula y cerrado con punto."""
    texto = texto.lstrip(".,;:—- ").rstrip(",;:—- ")
    if texto and texto[-1] not in ".!?»”)":
        texto += "."
    for posicion, letra in enumerate(texto):
        if letra.isalpha():
            texto = texto[:posicion] + letra.upper() + texto[posicion + 1 :]
            break
    return texto


def abrir(fuente: Path) -> sqlite3.Connection:
    if fuente.suffix == ".gz":
        destino = Path(tempfile.mkdtemp()) / fuente.stem
        with gzip.open(fuente, "rb") as origen, destino.open("wb") as salida:
            shutil.copyfileobj(origen, salida)
        fuente = destino
    return sqlite3.connect(f"file:{fuente}?mode=ro", uri=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("fuente", type=Path, help="NVI_es.SQLite3(.gz)")
    parser.add_argument("--salida", type=Path, default=SALIDA)
    args = parser.parse_args()

    base = abrir(args.fuente)
    libros = {
        sin_tildes(nombre): numero
        for numero, nombre in base.execute("SELECT book_number, long_name FROM books")
    }

    vistos: set[str] = set()
    filas: list[str] = []
    descartados: list[str] = []
    for linea in REFERENCIAS.read_text(encoding="utf-8").splitlines():
        linea = linea.strip()
        if not linea or linea.startswith("#"):
            continue
        cita = CITA.match(linea)
        if not cita:
            sys.exit(f"Cita mal escrita: {linea!r}")
        clave = sin_tildes(cita["libro"])
        numero = libros.get(ALIAS.get(clave, clave))
        if numero is None:
            sys.exit(f"Libro desconocido: {cita['libro']!r}")
        capitulo = int(cita["cap"])
        desde = int(cita["desde"])
        hasta = int(cita["hasta"] or desde)
        textos = [
            limpiar(texto)
            for (texto,) in base.execute(
                "SELECT text FROM verses WHERE book_number = ? AND chapter = ?"
                " AND verse BETWEEN ? AND ? ORDER BY verse",
                (numero, capitulo, desde, hasta),
            )
        ]
        if len(textos) != hasta - desde + 1 or not all(textos):
            sys.exit(f"No está en la base: {linea!r}")
        texto = como_frase(comillas(" ".join(textos)))
        referencia = f"{cita['libro']} {capitulo}:{desde}" + (f"-{hasta}" if hasta != desde else "")
        if referencia in vistos:
            continue
        vistos.add(referencia)
        if len(texto) > MAX_CARACTERES:
            descartados.append(f"{referencia} ({len(texto)})")
            continue
        if "\t" in texto or "\n" in texto:
            sys.exit(f"Separador dentro del texto: {referencia}")
        filas.append(f"{referencia}\t{texto}")
        if len(filas) == MAX_VERSICULOS:
            break

    encabezado = (
        "# Versículos del día para la barra superior. Generado por\n"
        "# scripts/biblia/extraer_versiculos.py; no editar a mano.\n"
        "# Texto bíblico tomado de la Santa Biblia, Nueva Versión Internacional® NVI®\n"
        "# © 1986, 1999, 2015 por Biblica, Inc.® Usado con permiso.\n"
        "# Reservados todos los derechos en todo el mundo.\n"
    )
    args.salida.write_text(encabezado + "\n".join(filas) + "\n", encoding="utf-8")
    tamaño = args.salida.stat().st_size
    print(f"{len(filas)} versículos -> {args.salida} ({tamaño / 1024:.1f} KB)")
    if descartados:
        print(f"Descartados por largos ({len(descartados)}): " + ", ".join(descartados))
    if len(filas) < MAX_VERSICULOS:
        print(f"Aviso: faltan {MAX_VERSICULOS - len(filas)} para cubrir el año")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
