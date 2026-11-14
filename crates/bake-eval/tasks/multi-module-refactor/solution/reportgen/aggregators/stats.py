"""Aggregators that describe the middle and the spread of the numbers."""



def mean(numbers):
    return sum(numbers) / len(numbers)


def median(numbers):
    ordered = sorted(numbers)
    middle = len(ordered) // 2
    if len(ordered) % 2:
        return ordered[middle]
    return (ordered[middle - 1] + ordered[middle]) / 2


def population_stdev(numbers):
    average = mean(numbers)
    return (sum((number - average) ** 2 for number in numbers) / len(numbers)).sqrt()
