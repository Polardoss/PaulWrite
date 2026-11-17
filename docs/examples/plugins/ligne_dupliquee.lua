-- Duplique la ligne du curseur sous elle-même. Les actions s'exécutent dans l'ordre : on place
-- d'abord le curseur en fin de ligne (commande intégrée), puis on insère la copie.
paul.command("exemple.dupliquer_ligne", "Exemple : dupliquer la ligne", function()
    local line = paul.cursor()
    local text = paul.line(line)
    paul.run("cursor.line_end")
    paul.insert("\n" .. text)
end)
