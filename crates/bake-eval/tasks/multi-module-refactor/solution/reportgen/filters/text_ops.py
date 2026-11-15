"""Text filters, ignoring case."""



def contains(cell, operand):
    return operand.lower() in str(cell).lower()


def starts_with(cell, operand):
    return str(cell).lower().startswith(operand.lower())
